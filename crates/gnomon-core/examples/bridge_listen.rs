//! Development listener: bind the bridge socket and print each snapshot.
//!
//! Stands in for the GUI so the bridge can be exercised without Chrome:
//!
//!     cargo run -p gnomon-core --example bridge_listen

use std::io::Write;

use gnomon_core::ipc;

fn main() {
    let path = match ipc::socket_path() {
        Ok(path) => path,
        Err(e) => {
            eprintln!("bridge_listen: {e}");
            std::process::exit(1);
        }
    };

    let listener = match ipc::listen(&path) {
        Ok(listener) => listener,
        Err(e) => {
            eprintln!("bridge_listen: {e}");
            std::process::exit(1);
        }
    };

    eprintln!("bridge_listen: listening on {}", path.display());

    ipc::serve(listener, |snapshot| {
        println!("received snapshot with {} windows", snapshot.windows.len());
        for window in &snapshot.windows {
            println!(
                "  {} {:.1}% {}",
                window.label(),
                window.percent,
                window.severity_class()
            );
        }
        // Stdout is block-buffered when redirected; flush so a piped or killed
        // run still shows the output.
        let _ = std::io::stdout().flush();
    });
}
