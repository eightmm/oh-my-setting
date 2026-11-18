//! A harness-free process is required by the exclusive subreaper contract.
use oms_supervisor::process::Reactor;
#[cfg(target_os = "linux")]
use oms_supervisor::process::{
    DuplexOptions, OutputStream, PhysicalCall, Progress, Request, Scope, StopReason, Streams,
};
#[cfg(target_os = "linux")]
use std::time::{Duration, Instant};

fn main() {
    #[cfg(not(target_os = "linux"))]
    {
        assert!(Reactor::dedicated().is_err());
        eprintln!("duplex WholeTree positive skipped: Linux exclusive subreaper unavailable");
    }
    #[cfg(target_os = "linux")]
    {
        for cancel in [false, true] {
            let mut reactor = Reactor::dedicated().expect("exclusive Linux fixture reactor");
            let script = if cancel {
                "(while :; do :; done) & printf ready; while :; do :; done"
            } else {
                "IFS= read -r line; cat >/dev/null; printf 'response:%s' \"$line\"; (while :; do :; done) & exit 0"
            };
            let mut request = Request::new(vec!["/bin/sh".into(), "-c".into(), script.into()]);
            request.scope = Scope::WholeTree;
            request.streams = Streams::Separate;
            request.deadline = Some(Instant::now() + Duration::from_secs(5));
            request.terminate_grace = Duration::ZERO;
            let call = PhysicalCall::new(if cancel {
                "duplex_tree_cancel"
            } else {
                "duplex_tree_eof"
            })
            .unwrap();
            request.physical_call = Some(call.clone());
            request.duplex = Some(DuplexOptions {
                max_frame_bytes: 128,
                max_input_bytes: 128,
                max_input_frames: 2,
                max_output_bytes: 128,
                max_output_events: 16,
                max_read_bytes_per_turn: 128,
                max_write_bytes_per_turn: 2,
            });
            let id = reactor.start(request).unwrap();
            if !cancel {
                reactor.send(id, b"hello\n").unwrap();
                reactor.close_input(id).unwrap();
            }
            let mut output = Vec::new();
            let mut stopped = false;
            let until = Instant::now() + Duration::from_secs(5);
            let done = loop {
                while let Some(event) = reactor.receive(id).unwrap() {
                    if event.stream == OutputStream::Stdout {
                        output.extend(event.bytes);
                    }
                }
                if cancel && !stopped && output == b"ready" {
                    reactor.stop(id, StopReason::Cancelled).unwrap();
                    stopped = true;
                }
                match reactor.poll(id, Duration::from_millis(10)).unwrap() {
                    Progress::Complete(done) => break done,
                    Progress::Pending(_) => assert!(
                        Instant::now() < until,
                        "exclusive fixture cleanup timed out"
                    ),
                }
            };
            assert!(done.cleanup_complete(), "{done:?}");
            assert!(done.all_reaped && done.leader_reaped);
            assert!(
                done.owned_cleanup()
                    .unwrap()
                    .matches(&call, Scope::WholeTree)
            );
            let terminal = done.duplex.as_ref().unwrap();
            for event in &terminal.output {
                if event.stream == OutputStream::Stdout {
                    output.extend_from_slice(&event.bytes);
                }
            }
            if cancel {
                assert!(stopped);
                assert_eq!(done.stop, Some(StopReason::Cancelled));
            } else {
                assert_eq!(output, b"response:hello");
                assert_eq!(done.stop, None);
                assert_eq!(done.exit.unwrap().shell_code(), 0);
                assert!(terminal.status.input_close_requested && terminal.status.input_drained);
            }
        }
        eprintln!("duplex WholeTree: real leader/descendant cleanup and cancellation passed");
    }
}
