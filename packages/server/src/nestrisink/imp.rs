use crate::messages::{MessageBase, MessageICE, MessageRaw, MessageSDP};
use crate::p2p::p2p::NestriConnection;
use crate::p2p::p2p_protocol_stream::NestriStreamProtocol;
use crate::proto::proto::proto_input::InputType::{
    KeyDown, KeyUp, MouseKeyDown, MouseKeyUp, MouseMove, MouseMoveAbs, MouseWheel,
};
use crate::proto::proto::{ProtoInput, ProtoMessageInput};
use atomic_refcell::AtomicRefCell;
use glib::subclass::prelude::*;
use gst::glib;
use gst::prelude::*;
use gst_webrtc::{WebRTCSDPType, WebRTCSessionDescription, gst_sdp};
use gstrswebrtc::signaller::{Signallable, SignallableImpl};
use parking_lot::RwLock as PLRwLock;
use prost::Message;
use std::sync::{Arc, LazyLock};
use webrtc::ice_transport::ice_candidate::RTCIceCandidateInit;
use webrtc::peer_connection::sdp::session_description::RTCSessionDescription;

pub struct Signaller {
    stream_room: PLRwLock<Option<String>>,
    stream_protocol: PLRwLock<Option<Arc<NestriStreamProtocol>>>,
    wayland_src: PLRwLock<Option<Arc<gst::Element>>>,
    data_channel: AtomicRefCell<Option<gst_webrtc::WebRTCDataChannel>>,
}
impl Default for Signaller {
    fn default() -> Self {
        Self {
            stream_room: PLRwLock::new(None),
            stream_protocol: PLRwLock::new(None),
            wayland_src: PLRwLock::new(None),
            data_channel: AtomicRefCell::new(None),
        }
    }
}
impl Signaller {
    pub async fn set_nestri_connection(
        &self,
        nestri_conn: NestriConnection,
    ) -> Result<(), Box<dyn std::error::Error>> {
        let stream_protocol = NestriStreamProtocol::new(nestri_conn).await?;
        *self.stream_protocol.write() = Some(Arc::new(stream_protocol));
        Ok(())
    }

    pub fn set_stream_room(&self, room: String) {
        *self.stream_room.write() = Some(room);
    }

    fn get_stream_protocol(&self) -> Option<Arc<NestriStreamProtocol>> {
        self.stream_protocol.read().clone()
    }

    pub fn set_wayland_src(&self, wayland_src: Arc<gst::Element>) {
        *self.wayland_src.write() = Some(wayland_src);
    }

    pub fn get_wayland_src(&self) -> Option<Arc<gst::Element>> {
        self.wayland_src.read().clone()
    }

    pub fn set_data_channel(&self, data_channel: gst_webrtc::WebRTCDataChannel) {
        match self.data_channel.try_borrow_mut() {
            Ok(mut dc) => *dc = Some(data_channel),
            Err(_) => gst::warning!(
                gst::CAT_DEFAULT,
                "Failed to set data channel - already borrowed"
            ),
        }
    }

    /// Helper method to clean things up
    fn register_callbacks(&self) {
        let Some(stream_protocol) = self.get_stream_protocol() else {
            gst::error!(gst::CAT_DEFAULT, "Stream protocol not set");
            return;
        };
        {
            let self_obj = self.obj().clone();
            stream_protocol.register_callback("answer", move |data| {
                if let Ok(message) = serde_json::from_slice::<MessageSDP>(&data) {
                    let sdp =
                        gst_sdp::SDPMessage::parse_buffer(message.sdp.sdp.as_bytes()).unwrap();
                    let answer = WebRTCSessionDescription::new(WebRTCSDPType::Answer, sdp);
                    self_obj.emit_by_name::<()>(
                        "session-description",
                        &[&"unique-session-id", &answer],
                    );
                } else {
                    gst::error!(gst::CAT_DEFAULT, "Failed to decode SDP message");
                }
            });
        }
        {
            let self_obj = self.obj().clone();
            stream_protocol.register_callback("ice-candidate", move |data| {
                if let Ok(message) = serde_json::from_slice::<MessageICE>(&data) {
                    let candidate = message.candidate;
                    let sdp_m_line_index = candidate.sdp_mline_index.unwrap_or(0) as u32;
                    let sdp_mid = candidate.sdp_mid;
                    self_obj.emit_by_name::<()>(
                        "handle-ice",
                        &[
                            &"unique-session-id",
                            &sdp_m_line_index,
                            &sdp_mid,
                            &candidate.candidate,
                        ],
                    );
                } else {
                    gst::error!(gst::CAT_DEFAULT, "Failed to decode ICE message");
                }
            });
        }
        {
            let self_obj = self.obj().clone();
            stream_protocol.register_callback("push-stream-ok", move |data| {
                if let Ok(answer) = serde_json::from_slice::<MessageRaw>(&data) {
                    // Decode room name string
                    if let Some(room_name) = answer.data.as_str() {
                        gst::info!(
                            gst::CAT_DEFAULT,
                            "Received OK answer for room: {}",
                            room_name
                        );
                    } else {
                        gst::error!(gst::CAT_DEFAULT, "Failed to decode room name from answer");
                    }

                    // Send our SDP offer
                    self_obj.emit_by_name::<()>(
                        "session-requested",
                        &[
                            &"unique-session-id",
                            &"consumer-identifier",
                            &None::<WebRTCSessionDescription>,
                        ],
                    );
                } else {
                    gst::error!(gst::CAT_DEFAULT, "Failed to decode answer");
                }
            });
        }
        {
            let self_obj = self.obj().clone();
            // After creating webrtcsink
            self_obj.connect_closure(
                "webrtcbin-ready",
                false,
                glib::closure!(move |signaller: &super::NestriSignaller,
                                     _consumer_identifier: &str,
                                     webrtcbin: &gst::Element| {
                    gst::info!(gst::CAT_DEFAULT, "Adding data channels");
                    // Create data channels on webrtcbin
                    let data_channel = Some(
                        webrtcbin.emit_by_name::<gst_webrtc::WebRTCDataChannel>(
                            "create-data-channel",
                            &[
                                &"nestri-data-channel",
                                &gst::Structure::builder("config")
                                    .field("ordered", &true)
                                    .field("max-retransmits", &2u32)
                                    .field("priority", "high")
                                    .field("protocol", "raw")
                                    .build(),
                            ],
                        ),
                    );
                    if let Some(data_channel) = data_channel {
                        gst::info!(gst::CAT_DEFAULT, "Data channel created");
                        if let Some(wayland_src) = signaller.imp().get_wayland_src() {
                            setup_data_channel(&data_channel, &*wayland_src);
                            signaller.imp().set_data_channel(data_channel);
                        } else {
                            gst::error!(gst::CAT_DEFAULT, "Wayland display source not set");
                        }
                    } else {
                        gst::error!(gst::CAT_DEFAULT, "Failed to create data channel");
                    }
                }),
            );
        }
    }
}
impl SignallableImpl for Signaller {
    fn start(&self) {
        gst::info!(gst::CAT_DEFAULT, "Signaller started");

        // Register message callbacks
        self.register_callbacks();

        // Subscribe to reconnection notifications
        // TODO: Re-implement reconnection handling

        let Some(stream_room) = self.stream_room.read().clone() else {
            gst::error!(gst::CAT_DEFAULT, "Stream room not set");
            return;
        };

        let push_msg = MessageRaw {
            base: MessageBase {
                payload_type: "push-stream-room".to_string(),
                latency: None,
            },
            data: serde_json::Value::from(stream_room),
        };

        let Some(stream_protocol) = self.get_stream_protocol() else {
            gst::error!(gst::CAT_DEFAULT, "Stream protocol not set");
            return;
        };

        if let Err(e) = stream_protocol.send_message(&push_msg) {
            tracing::error!("Failed to send push stream room message: {:?}", e);
        }
    }

    fn stop(&self) {
        gst::info!(gst::CAT_DEFAULT, "Signaller stopped");
    }

    fn send_sdp(&self, _session_id: &str, sdp: &WebRTCSessionDescription) {
        let sdp_message = MessageSDP {
            base: MessageBase {
                payload_type: "offer".to_string(),
                latency: None,
            },
            sdp: RTCSessionDescription::offer(sdp.sdp().as_text().unwrap()).unwrap(),
        };

        let Some(stream_protocol) = self.get_stream_protocol() else {
            gst::error!(gst::CAT_DEFAULT, "Stream protocol not set");
            return;
        };

        if let Err(e) = stream_protocol.send_message(&sdp_message) {
            tracing::error!("Failed to send SDP message: {:?}", e);
        }
    }

    fn add_ice(
        &self,
        _session_id: &str,
        candidate: &str,
        sdp_m_line_index: u32,
        sdp_mid: Option<String>,
    ) {
        let candidate_init = RTCIceCandidateInit {
            candidate: candidate.to_string(),
            sdp_mid,
            sdp_mline_index: Some(sdp_m_line_index as u16),
            ..Default::default()
        };
        let ice_message = MessageICE {
            base: MessageBase {
                payload_type: "ice-candidate".to_string(),
                latency: None,
            },
            candidate: candidate_init,
        };

        let Some(stream_protocol) = self.get_stream_protocol() else {
            gst::error!(gst::CAT_DEFAULT, "Stream protocol not set");
            return;
        };

        if let Err(e) = stream_protocol.send_message(&ice_message) {
            tracing::error!("Failed to send ICE candidate message: {:?}", e);
        }
    }

    fn end_session(&self, session_id: &str) {
        gst::info!(gst::CAT_DEFAULT, "Ending session: {}", session_id);
    }
}
#[glib::object_subclass]
impl ObjectSubclass for Signaller {
    const NAME: &'static str = "NestriSignaller";
    type Type = super::NestriSignaller;
    type ParentType = glib::Object;
    type Interfaces = (Signallable,);
}
impl ObjectImpl for Signaller {
    fn properties() -> &'static [glib::ParamSpec] {
        static PROPS: LazyLock<Vec<glib::ParamSpec>> = LazyLock::new(|| {
            vec![
                glib::ParamSpecBoolean::builder("manual-sdp-munging")
                    .nick("Manual SDP munging")
                    .blurb("Whether the signaller manages SDP munging itself")
                    .default_value(false)
                    .read_only()
                    .build(),
            ]
        });

        PROPS.as_ref()
    }
    fn property(&self, _id: usize, pspec: &glib::ParamSpec) -> glib::Value {
        match pspec.name() {
            "manual-sdp-munging" => false.to_value(),
            _ => unimplemented!(),
        }
    }
}

fn setup_data_channel(data_channel: &gst_webrtc::WebRTCDataChannel, wayland_src: &gst::Element) {
    let wayland_src = wayland_src.clone();

    data_channel.connect_on_message_data(move |_data_channel, data| {
        if let Some(data) = data {
            match ProtoMessageInput::decode(data.to_vec().as_slice()) {
                Ok(message_input) => {
                    if let Some(input_msg) = message_input.data {
                        // Process the input message and create an event
                        if let Some(event) = handle_input_message(input_msg) {
                            // Send the event to wayland source, result bool is ignored
                            let _ = wayland_src.send_event(event);
                        }
                    } else {
                        tracing::error!("Failed to parse InputMessage");
                    }
                }
                Err(e) => {
                    tracing::error!("Failed to decode MessageInput: {:?}", e);
                }
            }
        }
    });
}

fn handle_input_message(input_msg: ProtoInput) -> Option<gst::Event> {
    if let Some(input_type) = input_msg.input_type {
        match input_type {
            MouseMove(data) => {
                let structure = gst::Structure::builder("MouseMoveRelative")
                    .field("pointer_x", data.x as f64)
                    .field("pointer_y", data.y as f64)
                    .build();

                Some(gst::event::CustomUpstream::new(structure))
            }
            MouseMoveAbs(data) => {
                let structure = gst::Structure::builder("MouseMoveAbsolute")
                    .field("pointer_x", data.x as f64)
                    .field("pointer_y", data.y as f64)
                    .build();

                Some(gst::event::CustomUpstream::new(structure))
            }
            KeyDown(data) => {
                let structure = gst::Structure::builder("KeyboardKey")
                    .field("key", data.key as u32)
                    .field("pressed", true)
                    .build();

                Some(gst::event::CustomUpstream::new(structure))
            }
            KeyUp(data) => {
                let structure = gst::Structure::builder("KeyboardKey")
                    .field("key", data.key as u32)
                    .field("pressed", false)
                    .build();

                Some(gst::event::CustomUpstream::new(structure))
            }
            MouseWheel(data) => {
                let structure = gst::Structure::builder("MouseAxis")
                    .field("x", data.x as f64)
                    .field("y", data.y as f64)
                    .build();

                Some(gst::event::CustomUpstream::new(structure))
            }
            MouseKeyDown(data) => {
                let structure = gst::Structure::builder("MouseButton")
                    .field("button", data.key as u32)
                    .field("pressed", true)
                    .build();

                Some(gst::event::CustomUpstream::new(structure))
            }
            MouseKeyUp(data) => {
                let structure = gst::Structure::builder("MouseButton")
                    .field("button", data.key as u32)
                    .field("pressed", false)
                    .build();

                Some(gst::event::CustomUpstream::new(structure))
            }
        }
    } else {
        None
    }
}
