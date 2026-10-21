kind: read
area: kernel

Read the package `quota`, then answer each question with an answer block holding the answer alone.

- q1: After `charge(30)` and then `charge(80)` on `Quota(used: 0, limit: 100)`, what is its `used` field?
- q2: What does `Quota(used: 7, limit: 9).percent_used()` return?
- q3: If the last line of `percent_used` were `return self.used * 100 / self.limit`, which diagnostic code would `nan check` report on that line?
- q4: If that line were `return u32.saturate(f64.widen(self.used) * 100.0 / f64.widen(self.limit))`, which diagnostic code would `nan check` report on it?
