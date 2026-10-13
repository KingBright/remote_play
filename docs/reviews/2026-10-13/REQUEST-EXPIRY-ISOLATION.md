# Isolate local Session request timeouts

The client tracks pending controls by command category and numeric ID. The local retry timer previously discarded that category and rejected every pending request with the same number. An activity or source-switch timeout could therefore remove an unacknowledged ReleaseInput, stop its retransmission, and also cancel an independent Configure or source switch.

Local expiry now removes only the exact pending key. An expired source switch resets only its affected video handler so the retained source can recover from a keyframe. Other command acknowledgements and retransmissions remain pending. The timer sends the existing timeout error; wire formats, authentication, transport selection, UI and platform adapters are unchanged. Remote Error replies still use their existing conservative handling because their wire request ID does not identify a command category.

Three paired regressions use the same test bodies against the baseline and fix. They check actual loopback control datagrams with colliding IDs, gated source video and keyframe recovery, and the production actor retry timer retransmitting ReleaseInput to a separate loopback peer. Baseline: 0 passed / 3 failed, with successful compilation. Fix: 3 passed. No account profile, device media, desktop bus or OS input was used.

Validation on the existing managed MacBook cache: remote_core library 299 passed; release Python suite ran 287 tests with 7 skipped. The preexisting 104 primary worktree files retain their content and mtimes. Paired source and raw logs are saved in the task-local request-expiry-recovery-20261013 directory.

This establishes a client retry defect, not the cause of the historical physical two-Mac input failure. MacBook remains build 1086 and Studio build 1085; mixed addressed-P2P versions still need a compatible staged update before that physical direction can be revalidated. HO5 restart and first system Portal selection remain separate acceptance steps.
