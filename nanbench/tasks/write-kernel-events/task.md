kind: write
area: kernel

In the package `events`, write in its root module the three methods of `Log` that the module `locked` calls, each with the limits `notrap, terminates, noalloc`. `pub fn Log.record(mut self, event: Event)` adds the event as the newest; when the log already holds `CAPACITY` events, it drops the event instead and counts the drop, the count stopping at the largest `u64`. `pub fn Log.next(mut self) -> Option[Event]` removes and gives the oldest event, or none when the log is empty. `pub fn Log.dropped(self) -> u64` gives the number of events dropped so far.
