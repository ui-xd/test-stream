FROM docker.io/golang:1.24-alpine AS go-build
WORKDIR /builder
COPY packages/relay/ /builder/
RUN go build

FROM docker.io/golang:1.24-alpine
COPY --from=go-build /builder/relay /relay/relay
WORKDIR /relay

# TODO: Switch running layer to just alpine (doesn't need golang dev stack)

# ENV flags
ENV VERBOSE=false
ENV DEBUG=false
ENV ENDPOINT_PORT=8088
ENV MESH_PORT=8089
ENV WEBRTC_UDP_START=10000
ENV WEBRTC_UDP_END=20000
ENV STUN_SERVER="stun.l.google.com:19302"
ENV WEBRTC_UDP_MUX=8088
ENV WEBRTC_NAT_IPS=""
ENV AUTO_ADD_LOCAL_IP=true
ENV TLS_CERT=""
ENV TLS_KEY=""

EXPOSE $ENDPOINT_PORT
EXPOSE $MESH_PORT
EXPOSE $WEBRTC_UDP_START-$WEBRTC_UDP_END/udp
EXPOSE $WEBRTC_UDP_MUX/udp

ENTRYPOINT ["/relay/relay"]