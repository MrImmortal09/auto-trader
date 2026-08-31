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

### you can connect to the cloud server where the backend server is running by running `ssh ubuntu@140.245.209.140`
- It is a plain Ubuntu VM in **Hyderabad** (no cloud-vendor CLI involved) — the old `gcloud compute ssh ...` route is dead, do not use it.

### kotak api docs are present in kotak-api-docs folder

### Deployment Context
- The backend binary is located in `~/auto-trader/backend/` on the VM. It is a **version-stamped**
  file (e.g. `server-0.1.93-x86_64-unknown-linux-gnu`), not `./server` — `~/auto-trader/backend/server`
  is the *source crate directory*. Check `ps -eo pid,cmd | grep server-` for the one actually running.
- The server is executed natively inside a `tmux` session named `0` (specifically pane `0:0`).
- Restarting the server programmatically requires commands like
  `tmux send-keys -t 0:0 "cd ~/auto-trader/backend && ./server-<version>-x86_64-unknown-linux-gnu" C-m`
  since it is not managed by systemd.
- The VM has **954 MB RAM and no swap**, and stdout is only kept in the tmux scrollback (no log file),
  so a crash or freeze leaves little forensic trail beyond the `system_logs` table in `trades.db`.

### Verification Rule
- ALWAYS run `cargo build` (or `cargo check`) in the appropriate directory after making ANY changes to Rust code to verify compilation before concluding your turn.
- ALWAYS run `pnpm run build` in the `frontend` directory after making ANY changes to frontend code to verify there are no type or build errors.