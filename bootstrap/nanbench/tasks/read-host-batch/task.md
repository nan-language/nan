kind: read
area: host

Read the program `batch`, then answer each question with an answer block holding the answer alone. Each run below calls `run_batch` with the world `testing::world()` gives and the delays 30, 10 and 20, in that order.

- q1: With `limit_ms: 100`, what is the `last` field of the board `run_batch` gives?
- q2: With `limit_ms: 25`, what is the `finished` field of the board `run_batch` gives?
- q3: In that run with `limit_ms: 25`, how many milliseconds pass on the world's clock from the call of `run_batch` to its return?
- q4: Which diagnostic code does `nan check` report if the line `try await clock.sleep(1)` is added as the first line of the body of `await when mut mine`?
