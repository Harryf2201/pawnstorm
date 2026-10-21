# Real-opening starts for self-play data (gen7 on)

- **gen7-human.uci**: 100,000 opening lines (UCI moves from the start position, 8-16 plies), one per line.
  Source: Lichess database, September 2026 rated games (database.lichess.org, streamed; the first 12,076,585 games
  read). Kept: rated human-vs-human games (no BOT title), both players rated 2200+, base time at least 3 minutes,
  normal termination: 157,024 games; each gave its first 8-16 plies (seeded choice), deduplicated by the position
  reached (51,821 duplicates), and lines reaching a test-book start position removed (4,713). Moves only: no
  evaluations, names or annotations. Built with tools/openings (seed 7, min Elo 2200).
- **testbook-starts.fen**: the 34,700 start positions of the 8moves_v3 test book. Datagen's --deny skips any game
  whose opening (line plus random plies) reaches one, so self-play never starts from a test position.

Datagen usage: pawnstorm datagen <threads> <positions> <soft-nodes> <dir> [seconds] [seed]
  --openings openings/gen7-human.uci --opening-percent 50 --deny openings/testbook-starts.fen
(50% of games start from a real line plus 0-4 random plies, the rest from 8-9 random plies.)
