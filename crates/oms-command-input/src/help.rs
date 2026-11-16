//! argparse usage and help text of the canonical `oms input` tree (COLUMNS=80).
pub const TEXTS: &[(&str, &str, &str)] = &[
    (
        "oms input",
        "usage: oms input [-h] [--repo REPO] [--json]\n                 {attach,attachment,attachments,add,edit,move,list,show,preview,cancel,deliver,reconcile,feedback}\n                 ...\n",
        "usage: oms input [-h] [--repo REPO] [--json]\n                 {attach,attachment,attachments,add,edit,move,list,show,preview,cancel,deliver,reconcile,feedback}\n                 ...\n\nCapture source-aware attachments and edit local steer/next inputs before\nexplicit room delivery.\n\npositional arguments:\n  {attach,attachment,attachments,add,edit,move,list,show,preview,cancel,deliver,reconcile,feedback}\n\noptions:\n  -h, --help            show this help message and exit\n  --repo REPO\n  --json                JSON output (default)\n",
    ),
    (
        "oms input attach",
        "usage: oms input attach [-h] [--repo REPO] [--json]\n                        {file,selection,terminal} ...\n",
        "usage: oms input attach [-h] [--repo REPO] [--json]\n                        {file,selection,terminal} ...\n\npositional arguments:\n  {file,selection,terminal}\n\noptions:\n  -h, --help            show this help message and exit\n  --repo REPO\n  --json                JSON output (default)\n",
    ),
    (
        "oms input attach file",
        "usage: oms input attach file [-h] [--repo REPO] [--json]\n                             [--start-line START_LINE] [--end-line END_LINE]\n                             path\n",
        "usage: oms input attach file [-h] [--repo REPO] [--json]\n                             [--start-line START_LINE] [--end-line END_LINE]\n                             path\n\npositional arguments:\n  path\n\noptions:\n  -h, --help            show this help message and exit\n  --repo REPO\n  --json                JSON output (default)\n  --start-line START_LINE\n  --end-line END_LINE\n",
    ),
    (
        "oms input attach selection",
        "usage: oms input attach selection [-h] [--repo REPO] [--json] --source\n                                  {browser,chat,text} --origin ORIGIN\n                                  (--text TEXT | --text-file TEXT_FILE)\n",
        "usage: oms input attach selection [-h] [--repo REPO] [--json] --source\n                                  {browser,chat,text} --origin ORIGIN\n                                  (--text TEXT | --text-file TEXT_FILE)\n\noptions:\n  -h, --help            show this help message and exit\n  --repo REPO\n  --json                JSON output (default)\n  --source {browser,chat,text}\n  --origin ORIGIN\n  --text TEXT\n  --text-file TEXT_FILE\n",
    ),
    (
        "oms input attach terminal",
        "usage: oms input attach terminal [-h] [--repo REPO] [--json] --pane PANE\n                                 [--lines LINES]\n",
        "usage: oms input attach terminal [-h] [--repo REPO] [--json] --pane PANE\n                                 [--lines LINES]\n\noptions:\n  -h, --help     show this help message and exit\n  --repo REPO\n  --json         JSON output (default)\n  --pane PANE\n  --lines LINES\n",
    ),
    (
        "oms input attachment",
        "usage: oms input attachment [-h] [--repo REPO] [--json] [--full] id\n",
        "usage: oms input attachment [-h] [--repo REPO] [--json] [--full] id\n\npositional arguments:\n  id\n\noptions:\n  -h, --help   show this help message and exit\n  --repo REPO\n  --json       JSON output (default)\n  --full       return the full bounded snapshot instead of its first 8 KiB\n",
    ),
    (
        "oms input attachments",
        "usage: oms input attachments [-h] [--repo REPO] [--json]\n",
        "usage: oms input attachments [-h] [--repo REPO] [--json]\n\noptions:\n  -h, --help   show this help message and exit\n  --repo REPO\n  --json       JSON output (default)\n",
    ),
    (
        "oms input add",
        "usage: oms input add [-h] [--repo REPO] [--json] --room ROOM --from SENDER\n                     --to TO [--mode {steer,next}]\n                     (--text TEXT | --text-file TEXT_FILE)\n                     [--attachment ATTACHMENT]\n",
        "usage: oms input add [-h] [--repo REPO] [--json] --room ROOM --from SENDER\n                     --to TO [--mode {steer,next}]\n                     (--text TEXT | --text-file TEXT_FILE)\n                     [--attachment ATTACHMENT]\n\noptions:\n  -h, --help            show this help message and exit\n  --repo REPO\n  --json                JSON output (default)\n  --room ROOM\n  --from SENDER\n  --to TO\n  --mode {steer,next}\n  --text TEXT\n  --text-file TEXT_FILE\n  --attachment ATTACHMENT\n",
    ),
    (
        "oms input edit",
        "usage: oms input edit [-h] [--repo REPO] [--json]\n                      [--text TEXT | --text-file TEXT_FILE]\n                      [--mode {steer,next}] [--attachment ATTACHMENT]\n                      [--clear-attachments]\n                      id\n",
        "usage: oms input edit [-h] [--repo REPO] [--json]\n                      [--text TEXT | --text-file TEXT_FILE]\n                      [--mode {steer,next}] [--attachment ATTACHMENT]\n                      [--clear-attachments]\n                      id\n\npositional arguments:\n  id\n\noptions:\n  -h, --help            show this help message and exit\n  --repo REPO\n  --json                JSON output (default)\n  --text TEXT\n  --text-file TEXT_FILE\n  --mode {steer,next}\n  --attachment ATTACHMENT\n                        replace the entire attachment list\n  --clear-attachments\n",
    ),
    (
        "oms input move",
        "usage: oms input move [-h] [--repo REPO] [--json] [--before BEFORE] id\n",
        "usage: oms input move [-h] [--repo REPO] [--json] [--before BEFORE] id\n\npositional arguments:\n  id\n\noptions:\n  -h, --help       show this help message and exit\n  --repo REPO\n  --json           JSON output (default)\n  --before BEFORE  queued item ID; omit to move to the end\n",
    ),
    (
        "oms input list",
        "usage: oms input list [-h] [--repo REPO] [--json]\n",
        "usage: oms input list [-h] [--repo REPO] [--json]\n\noptions:\n  -h, --help   show this help message and exit\n  --repo REPO\n  --json       JSON output (default)\n",
    ),
    (
        "oms input show",
        "usage: oms input show [-h] [--repo REPO] [--json] id\n",
        "usage: oms input show [-h] [--repo REPO] [--json] id\n\npositional arguments:\n  id\n\noptions:\n  -h, --help   show this help message and exit\n  --repo REPO\n  --json       JSON output (default)\n",
    ),
    (
        "oms input preview",
        "usage: oms input preview [-h] [--repo REPO] [--json] id\n",
        "usage: oms input preview [-h] [--repo REPO] [--json] id\n\npositional arguments:\n  id\n\noptions:\n  -h, --help   show this help message and exit\n  --repo REPO\n  --json       JSON output (default)\n",
    ),
    (
        "oms input cancel",
        "usage: oms input cancel [-h] [--repo REPO] [--json] id\n",
        "usage: oms input cancel [-h] [--repo REPO] [--json] id\n\npositional arguments:\n  id\n\noptions:\n  -h, --help   show this help message and exit\n  --repo REPO\n  --json       JSON output (default)\n",
    ),
    (
        "oms input deliver",
        "usage: oms input deliver [-h] [--repo REPO] [--json] [--apply] [--at-boundary]\n                         [--expected-sha256 EXPECTED_SHA256]\n                         id\n",
        "usage: oms input deliver [-h] [--repo REPO] [--json] [--apply] [--at-boundary]\n                         [--expected-sha256 EXPECTED_SHA256]\n                         id\n\npositional arguments:\n  id\n\noptions:\n  -h, --help            show this help message and exit\n  --repo REPO\n  --json                JSON output (default)\n  --apply               persist a message; otherwise preview only\n  --at-boundary         explicit next-task boundary assertion\n  --expected-sha256 EXPECTED_SHA256\n                        require this exact previewed payload\n",
    ),
    (
        "oms input reconcile",
        "usage: oms input reconcile [-h] [--repo REPO] [--json] id\n",
        "usage: oms input reconcile [-h] [--repo REPO] [--json] id\n\npositional arguments:\n  id\n\noptions:\n  -h, --help   show this help message and exit\n  --repo REPO\n  --json       JSON output (default)\n",
    ),
    (
        "oms input feedback",
        "usage: oms input feedback [-h] [--repo REPO] [--json] --batch BATCH --room\n                          ROOM --from SENDER --to TO [--mode {steer,next}]\n",
        "usage: oms input feedback [-h] [--repo REPO] [--json] --batch BATCH --room\n                          ROOM --from SENDER --to TO [--mode {steer,next}]\n\noptions:\n  -h, --help           show this help message and exit\n  --repo REPO\n  --json               JSON output (default)\n  --batch BATCH        exact imported artifact-view feedback batch SHA-256\n  --room ROOM\n  --from SENDER\n  --to TO\n  --mode {steer,next}\n",
    ),
];
