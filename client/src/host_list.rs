use gpui::*;
use remote_core::net::DEFAULT_CONTROL_PORT;
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
            hosts: vec![SocketAddr::from(([127, 0, 0, 1], DEFAULT_CONTROL_PORT))],
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
            .bg(rgb(0x101114))
            .p_8();

        list = list.child(
            div()
                .text_xl()
                .text_color(rgb(0xf4f7fb))
                .mb_6()
                .child("RemotePlay"),
        );

        for host in &self.hosts {
            let host_addr = *host;
            list = list.child(
                div()
                    .flex()
                    .justify_between()
                    .p_4()
                    .mb_2()
                    .bg(rgb(0x181b20))
                    .border_1()
                    .border_color(rgb(0x2a3038))
                    .rounded_md()
                    .child(div().text_color(rgb(0xf4f7fb)).child(host.to_string()))
                    .child(
                        div()
                            .id(format!("connect-{}", host_addr))
                            .bg(rgb(0x2563eb))
                            .p_2()
                            .rounded_sm()
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
