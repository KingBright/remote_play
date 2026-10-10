//! Manual control-plane entry. No action occurs without an explicit --select-*.
//! Selection/FD ownership is not a captured, encoded or presented frame.

#[cfg(not(target_os = "linux"))]
fn main() {
    eprintln!("This manual portal control check requires Linux; no portal was called.");
}

#[cfg(target_os = "linux")]
#[tokio::main]
async fn main() -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    use host::{PickerSources, request_local_portal_capture};
    let mut sources = None;
    let mut parent = String::new();
    for argument in std::env::args().skip(1) {
        match argument.as_str() {
            "--select-window" if sources.is_none() => sources = Some(PickerSources::Window),
            "--select-monitor" if sources.is_none() => sources = Some(PickerSources::Monitor),
            value if value.starts_with("--parent=") => parent = value[9..].to_owned(),
            _ => return Err("use exactly one --select-window/--select-monitor and optional --parent=wayland:HANDLE or x11:XID".into()),
        }
    }
    let Some(sources) = sources else {
        eprintln!(
            "No selection requested. Pass --select-window or --select-monitor to open the normal local system picker; no portal was called."
        );
        return Ok(());
    };
    let (cancel, cancel_rx) = tokio::sync::watch::channel(false);
    let cancel_task = tokio::spawn(async move {
        if tokio::signal::ctrl_c().await.is_ok() {
            cancel.send_replace(true);
        }
        std::future::pending::<()>().await;
    });
    let result = request_local_portal_capture(&parent, sources, cancel_rx).await;
    match result {
        Ok(mut lease) => {
            let selected = lease.selected_source();
            println!(
                "Selected {:?}; logical size {:?}; capture pixels and presentation remain unverified.",
                selected.kind, selected.logical_size
            );
            let (_, fd) = lease.take_pipewire_remote()?;
            drop(fd); // This check never connects a global or restricted PipeWire remote.
            let closed = lease.close().await;
            println!("Restricted FD returned once; Close RPC receipt={closed}.");
        }
        Err(error) => {
            cancel_task.abort();
            return Err(error.into());
        }
    }
    cancel_task.abort();
    Ok(())
}
