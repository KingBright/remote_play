use gpui::*;
use std::net::SocketAddr;

pub enum HostListEvent {
    Connect(SocketAddr),
}

pub struct HostListView {
    hosts: Vec<SocketAddr>,
}

impl HostListView {
    pub fn new(_cx: &mut gpui::Context<Self>) -> Self {
        Self {
            hosts: vec![
                "127.0.0.1:8000".parse().unwrap(),
                "192.168.1.100:8000".parse().unwrap(),
            ],
        }
    }
}

impl gpui::EventEmitter<HostListEvent> for HostListView {}

impl Render for HostListView {
    fn render(
        &mut self,
        _window: &mut gpui::Window,
        cx: &mut gpui::Context<Self>,
    ) -> impl IntoElement {
        let mut list = div()
            .flex()
            .flex_col()
            .w_full()
            .h_full()
            .bg(rgb(0x1e1e1e))
            .p_8();

        list = list.child(
            div()
                .text_xl()
                .text_color(rgb(0xffffff))
                .mb_6()
                .child("Select a Host to Connect"),
        );

        for host in &self.hosts {
            let host_addr = host.clone();
            list = list.child(
                div()
                    .flex()
                    .justify_between()
                    .p_4()
                    .mb_2()
                    .bg(rgb(0x2d2d2d))
                    .rounded_md()
                    .child(div().text_color(rgb(0xffffff)).child(host.to_string()))
                    .child(
                        div()
                            .id(format!("connect-{}", host_addr))
                            .bg(rgb(0x007acc))
                            .p_2()
                            .rounded_md()
                            .text_color(rgb(0xffffff))
                            .cursor_pointer()
                            // FIXME: adjust closure arguments if needed.
                            .on_click(cx.listener(move |_this, _event, _window, cx| {
                                cx.emit(HostListEvent::Connect(host_addr));
                            }))
                            .child("Connect"),
                    ),
            );
        }

        list
    }
}
