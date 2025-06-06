import { JSX, ParentProps, Show, createContext, useContext } from "solid-js";

export function createInitializedContext<
    Name extends string,
    T extends { ready: boolean }
>(name: Name, cb: () => T) {
    const ctx = createContext<T>();

    return {
        use: () => {
            const context = useContext(ctx);
            if (!context) throw new Error(`No ${name} context`);
            return context;
        },
        provider: (props: ParentProps & { loadingUI?: JSX.Element }) => {
            const value = cb();
            return (
                <Show
                    fallback={props.loadingUI}
                    when={value.ready}>
                    <ctx.Provider value={value} {...props}>
                        {props.children}
                    </ctx.Provider>
                </Show>
            );
        },
    }
}