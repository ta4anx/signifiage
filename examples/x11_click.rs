//! Send one left click at absolute X11 screen coordinates (XTest).
//!
//! ```text
//! cargo run --release --example x11_click -- 700 40
//! ```
//!
//! Development aid for interaction checks on the X11/XWayland session.

use anyhow::{Context, Result};
use x11rb::connection::Connection;
use x11rb::protocol::xtest::ConnectionExt as _;

fn main() -> Result<()> {
    let mut args = std::env::args().skip(1);
    let x: i16 = args
        .next()
        .context("usage: x11_click <x> <y> [button]")?
        .parse()
        .context("x must be a number")?;
    let y: i16 = args
        .next()
        .context("usage: x11_click <x> <y> [button]")?
        .parse()
        .context("y must be a number")?;
    let button: u8 = args
        .next()
        .map(|value| value.parse())
        .transpose()?
        .unwrap_or(1);

    let (connection, screen) = x11rb::connect(None).context("cannot connect to $DISPLAY")?;
    let root = connection.setup().roots[screen].root;
    connection
        .xtest_fake_input(6, 0, 0, root, x, y, 0)
        .context("motion")?;
    connection.flush()?;
    std::thread::sleep(std::time::Duration::from_millis(60));
    connection
        .xtest_fake_input(4, button, 0, root, x, y, 0)
        .context("press")?;
    connection.flush()?;
    std::thread::sleep(std::time::Duration::from_millis(40));
    connection
        .xtest_fake_input(5, button, 0, root, x, y, 0)
        .context("release")?;
    connection.flush()?;
    println!("clicked {x},{y} button {button}");
    Ok(())
}
