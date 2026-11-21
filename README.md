# Bacteria

A two-player bacteria game in the browser. One Rust binary serves the page and runs the games.

- **Play with a friend**: create a game, share the link, it starts when they open it (like Lichess).
- **Play the computer**: easy, medium or hard.
- **Pass and play**: two players on the same screen.
- **Past games**: every finished game is saved. Replay it move by move and see what each move was worth, what the best move was, and why.

## Rules

Bacteria is played on 8x8 boards. Control more cells to win.

- Tap one of your bacteria, then an empty cell:
  - any neighbouring cell, diagonals included: the bacterium **copies** itself;
  - exactly two cells away in a straight line (no diagonals): it **jumps** (over anything) and its old cell empties.
- Where it lands, it **converts** every enemy bacterium around it, diagonals included.
- **30 seconds per move**. Run out and you lose. If one of the first two moves isn't played in time, the game is aborted.
- When the player to move has no legal move, the game ends and every empty cell goes to the other player. The player with the most cells wins.

## Run it

```sh
docker compose up -d --build     # http://localhost:3000
```
