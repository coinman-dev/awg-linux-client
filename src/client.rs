//! GUI side of the helper connection.

use crate::proto::{READY, Request, Response};
use std::{
    io::{BufRead, BufReader, Write},
    process::{Child, Command, Stdio},
    sync::mpsc::{Receiver, Sender, channel},
    thread,
};

pub enum Event {
    Ready,
    Reply(Request, Response),
    /// Helper exited / authorization was cancelled.
    Died(String),
}

pub struct Helper {
    tx: Sender<Request>,
    pub rx: Receiver<Event>,
    child: Child,
    pub ready: bool,
}

impl Helper {
    pub fn spawn(repaint: impl Fn() + Send + 'static) -> Result<Helper, String> {
        let exe = std::env::current_exe().map_err(|e| e.to_string())?;
        let mut child = Command::new("pkexec")
            .arg(&exe)
            .arg("--helper")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .map_err(|e| format!("cannot run pkexec: {e}"))?;
        let mut stdin = child.stdin.take().unwrap();
        let mut stdout = BufReader::new(child.stdout.take().unwrap());
        let stderr = child.stderr.take().unwrap();
        let (tx, req_rx) = channel::<Request>();
        let (ev_tx, rx) = channel::<Event>();

        let err_buf = std::sync::Arc::new(std::sync::Mutex::new(String::new()));
        let eb = err_buf.clone();
        thread::spawn(move || {
            for l in BufReader::new(stderr).lines().map_while(Result::ok) {
                let mut b = eb.lock().unwrap();
                b.push_str(&l);
                b.push('\n');
            }
        });

        thread::spawn(move || {
            let died = |why: &str| {
                let stderr = err_buf.lock().unwrap().trim().to_string();
                let msg = if stderr.is_empty() { why.to_string() } else { format!("{why}: {stderr}") };
                let _ = ev_tx.send(Event::Died(msg));
                repaint();
            };
            let mut line = String::new();
            match stdout.read_line(&mut line) {
                Ok(n) if n > 0 && line.trim() == READY => {
                    let _ = ev_tx.send(Event::Ready);
                    repaint();
                }
                _ => {
                    // give the stderr reader a moment to collect pkexec's message
                    thread::sleep(std::time::Duration::from_millis(100));
                    return died("authorization failed or cancelled");
                }
            }
            for req in req_rx {
                let json = serde_json::to_string(&req).unwrap();
                if writeln!(stdin, "{json}").and_then(|_| stdin.flush()).is_err() {
                    return died("helper exited");
                }
                line.clear();
                match stdout.read_line(&mut line) {
                    Ok(n) if n > 0 => {
                        let resp = serde_json::from_str::<Response>(&line).unwrap_or_else(|e| Response {
                            ok: false,
                            error: Some(format!("bad helper reply: {e}")),
                            ..Default::default()
                        });
                        let _ = ev_tx.send(Event::Reply(req, resp));
                        repaint();
                    }
                    _ => return died("helper exited"),
                }
            }
        });

        Ok(Helper { tx, rx, child, ready: false })
    }

    pub fn send(&self, req: Request) -> bool {
        self.tx.send(req).is_ok()
    }
}

impl Drop for Helper {
    fn drop(&mut self) {
        // Closing stdin (dropping tx ends the writer loop) makes the helper exit.
        // We cannot signal a root process; just reap it if it's already gone.
        let _ = self.child.try_wait();
    }
}
