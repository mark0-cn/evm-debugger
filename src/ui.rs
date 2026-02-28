use dioxus::prelude::*;

pub fn app() -> Element {
    rsx! {
        div {
            style: "padding: 16px; font-family: ui-sans-serif, system-ui;",
            h1 { "EVM Debugger" }
            p { "Dioxus LiveView UI (WIP)." }
            p { "API is still available under /api/*." }
        }
    }
}
