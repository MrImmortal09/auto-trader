### Ask before writing code
- This project places real orders on a live Kotak Neo brokerage account. **Real money is at stake.**
- If anything about a design is unclear, **ask first — do not write code on a guess.** Ask as many questions as you need. A wrong assumption here is not a refactor you can redo; it can lose money.
- Do not treat earlier written-down decisions as settled just because they are written down. Several have already been reversed after review — re-confirm anything that changes order behaviour.
- While waiting on an answer, build the parts that are already settled rather than blocking on everything.
- When a judgment call has to be made anyway, pick the option that errs toward **not trading** or toward **being flat**, and state the assumption explicitly in your reply.

### Never sell more than we hold
- The brokerage account **carries no margin**. It buys options only — it never sells or shorts options or futures, and never buys futures.
- `resting stop qty + in-flight sell qty <= executed_qty` must hold at all times. Cancel or shrink the resting stop **before** sending another sell, and if that cancel or shrink fails, send **no sell at all**.
- An accidental oversell is not a bad fill, it is a naked short in an account with no margin to carry it.

### Inform the user if there is a Change in db and clearing the whole db is required.

### you can connet to the cloud server where the backend server is running by running  "gcloud compute ssh --zone \"us-east1-d\" \"trader-1\" --project \"trader-502418\"

### kotak api docs are present in kotak-api-docs folder

### Deployment Context
- The backend binary is located at `~/auto-trader/backend/server` on the GCP instance.
- The server is executed natively inside a `tmux` session named `0` (specifically pane `0:0`).
- Restarting the server programmatically requires commands like `tmux send-keys -t 0:0 "cd ~/auto-trader/backend && ./server" C-m` since it is not managed by systemd.

### Verification Rule
- ALWAYS run `cargo build` (or `cargo check`) in the appropriate directory after making ANY changes to Rust code to verify compilation before concluding your turn.
- ALWAYS run `pnpm run build` in the `frontend` directory after making ANY changes to frontend code to verify there are no type or build errors.