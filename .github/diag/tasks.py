import lldb

NAMES = {0: 'NEW', 1: 'READY', 2: 'RUNNING', 3: 'PARKING', 4: 'PARKED', 5: 'NOTIFIED', 6: 'ENDED', 7: 'STOPPING', 8: 'STOPPED'}


def dump(debugger, command, result, internal_dict):
    target = debugger.GetSelectedTarget()
    process = target.GetProcess()
    error = lldb.SBError()

    def q(address):
        return process.ReadUnsignedFromMemory(address, 8, error)

    def d(address):
        return process.ReadUnsignedFromMemory(address, 4, error)

    state = None
    for name in ('_NSstate', '__NSstate'):
        for found in target.FindSymbols(name):
            address = found.GetSymbol().GetStartAddress().GetLoadAddress(target)
            if address != lldb.LLDB_INVALID_ADDRESS:
                state = address
    if state is None:
        print('no runtime state symbol')
        return
    print('ready_head %#x threads %d timers %#x tasks_lock %d timer_lock %d' % (q(state + 16), q(state + 40), q(state + 208), q(state + 128), q(state + 136)))
    threads = q(state + 32)
    for index in range(q(state + 40)):
        record = q(threads + 8 * index)
        print('thread %d %#x current %#x idle %d locks %d' % (index, record, q(record + 24), q(record + 48), q(record + 72)))
    timer = q(state + 208)
    seen = 0
    while timer and seen < 50:
        print('timer %#x task %#x due %d done %d kind %d' % (timer, q(timer), q(timer + 80), q(timer + 16), q(timer + 40)))
        timer = q(timer + 48)
        seen += 1
    record = q(state + 120)
    seen = 0
    while record and seen < 200:
        io = q(record + 136)
        region = q(record + 56)
        line = 'task %#x state %s io %#x stop %d region %#x domain %#x wait %#x thread %#x' % (record, NAMES.get(d(record + 24), d(record + 24)), io, q(record + 240), region, q(record + 48), q(record + 256), q(record + 168))
        if region:
            line += ' region[id %d cancelled %d live %d]' % (q(region), q(region + 16), q(region + 32))
        if io > 1:
            op = io & ~7
            line += ' op[done %d cancel %d kind %d waiter %d link %d]' % (q(op + 16), q(op + 32), q(op + 40), q(op + 64), q(op + 72))
        print(line)
        record = q(record + 120)
        seen += 1


def __lldb_init_module(debugger, internal_dict):
    debugger.HandleCommand('command script add -f tasks.dump nan_tasks')
