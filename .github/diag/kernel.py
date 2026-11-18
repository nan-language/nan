"""Dumps a stalled kernel_steals boot's scheduler through QEMU's QMP socket.

Usage: kernel.py <monitor socket> <state address> <cpu records address>
"""
import json
import os
import re
import socket
import sys

NAMES = {0: 'NEW', 1: 'READY', 2: 'RUNNING', 3: 'PARKING', 4: 'PARKED', 5: 'NOTIFIED', 6: 'ENDED',
         7: 'STOPPING', 8: 'STOPPED', 9: 'PREEMPTED', 10: 'PREEMPTED_NOTIFIED'}

monitor = socket.socket(socket.AF_UNIX)
os.chdir(os.path.dirname(sys.argv[1]))
monitor.connect(os.path.basename(sys.argv[1]))
stream = monitor.makefile('rw')


def call(command, **arguments):
    stream.write(json.dumps({'execute': command, 'arguments': arguments}) + '\n')
    stream.flush()
    while True:
        reply = json.loads(stream.readline())
        if 'return' in reply or 'error' in reply:
            return reply.get('return', reply.get('error'))


stream.readline()
call('qmp_capabilities')
call('stop')


def hmp(line):
    return call('human-monitor-command', **{'command-line': line})


def q(address):
    text = hmp('xp/1gx %#x' % address)
    found = re.search(r':\s*(0x[0-9a-f]+)', text)
    return int(found.group(1), 16) if found else 0


def d(address):
    return q(address) & 0xffffffff


state, cpus = int(sys.argv[2], 16), int(sys.argv[3], 16)
count = q(state + 40)
print('cpus %d timers %#x tasks_lock %d timer_lock %d' % (count, q(state + 208), q(state + 128), q(state + 136)))
for index in range(count):
    cpu = cpus + 512 * index
    thread = q(cpu)
    print('cpu %d thread %#x current %#x locks %d timer_need %d queue %#x head_key %d running_key %d slice_end %d idle %d hook %d deadline %d frame %#x' % (
        index, thread, q(thread + 24), q(thread + 72), q(thread + 176), q(cpu + 288), q(cpu + 304), q(cpu + 312), q(cpu + 320), q(cpu + 328), q(cpu + 336), q(cpu + 344), q(cpu + 240)))
record = q(state + 120)
seen = 0
while record and seen < 64:
    line = 'task %#x state %s queue %#x key %d cpu %d preempted %d ran %d io %#x next %#x' % (
        record, NAMES.get(d(record + 24), d(record + 24)), q(record + 336), q(record + 320), q(record + 328), q(record + 280), q(record + 312), q(record + 136), q(record + 112))
    wait = q(record + 256)
    if wait:
        signal = q(wait + 8)
        line += ' wait[signal %#x lock %d stored %d waiter %#x]' % (signal, q(signal), q(signal + 8) & 0xff, q(signal + 16))
    print(line)
    record = q(record + 120)
    seen += 1
print(hmp('info registers -a'))
call('cont')
