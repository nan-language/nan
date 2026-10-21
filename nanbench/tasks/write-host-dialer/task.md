kind: write
area: host

In the program `dialer`, write `pub fn connect_with_backoff(net: sys::Net, clock: sys::Clock, port: u16, attempts: u32) -> sys::net::Conn fails ConnectError uses wait`, which `main` calls. It connects to `port` on `localhost`, making at most `attempts` attempts, and returns the first connection made. After each refused attempt (`sys::net::NetError.Refused`) but the last, it sleeps on `clock` before the next attempt: 100 ms after the first refusal, then twice as long as the previous pause, but never more than 1_600 ms. When every attempt was refused, including when `attempts` is 0, it fails with `ConnectError.GaveUp`; any other failure of an attempt fails it at once.
