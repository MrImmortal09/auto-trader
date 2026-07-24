### Inform the user if there is a Change in db and clearing the whole db is required.

### you can connet to the cloud server where the backend server is running by running  "gcloud compute ssh --zone \"us-east1-d\" \"trader-1\" --project \"trader-502418\"

### Deployment Context
- The backend binary is located at `~/auto-trader/backend/server` on the GCP instance.
- The server is executed natively inside a `tmux` session named `0` (specifically pane `0:0`).
- Restarting the server programmatically requires commands like `tmux send-keys -t 0:0 "cd ~/auto-trader/backend && ./server" C-m` since it is not managed by systemd.

### Verification Rule
- ALWAYS run `cargo build` (or `cargo check`) in the appropriate directory after making ANY changes to Rust code to verify compilation before concluding your turn.