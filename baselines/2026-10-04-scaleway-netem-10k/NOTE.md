# Invalid as a measure of the game: netem slowed the server

netem was on the server box's interface to the bots (and the bot box's), as
`baseline.sh netem` did with BOTS_SSH at the time. At 10k players a netem
qdisc is one queue behind one lock, holding hundreds of thousands of delayed
packets, and all 64 sending threads contended for it: egress went from
2.75 ms (clean) to 24-27 ms p50, the tick from 12 to 39-41 ms, and the ladder
fell to levels 6-7. The stand-ins, corrections and resyncs here follow from
that overload, not from the simulated network.

The clean rows are valid. The fix: shape only on the bot box (egress for
bots -> server, an ifb device on its ingress for server -> bots), leaving the
server's send path untouched. The 1k results in 2026-10-04-wsl2-netem are
unaffected (the queue never got near this load).
