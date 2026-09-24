"""Owned-process procd model for whole reload tests; no router/kernel writes."""
import json
import os
from pathlib import Path
import socket
import subprocess
import sys
import threading
import time

root = Path(sys.argv[2])
if not root.is_absolute() or (root / 'fixture.owner').read_text() != 'cake-reload-fixture-v1\n':
    raise SystemExit(90)
endpoint = str(root / 'procd.sock')

if sys.argv[1] == 'client':
    with socket.socket(socket.AF_UNIX) as client:
        client.settimeout(10)
        client.connect(endpoint)
        client.sendall(json.dumps(sys.argv[3:]).encode() + b'\n')
        with client.makefile('rb') as stream:
            response = json.loads(stream.readline(65536))
        print(json.dumps(response['body']))
        raise SystemExit(response['exit'])

if sys.argv[1] != 'supervise':
    raise SystemExit(91)

children = {}
definitions = {}
actions = []
statuses = {}
generation_env = 'CAKE_AUTORATE_SERVICE_CONFIG_ID'
daemon = '/usr/sbin/cake-autorated'
mode = os.environ['CAKE_RELOAD_FIXTURE_MODE']


def status(name, state):
    now = time.time()
    path = root / 'run' / name / 'status.json'
    path.parent.mkdir(exist_ok=True)
    started = statuses.get(name, {}).get('started_at', now)
    statuses[name] = dict(instance=name, state=state, started_at=started, updated_at=now)
    temporary = path.with_suffix('.tmp')
    temporary.write_text(json.dumps(statuses[name]))
    temporary.chmod(0o600)
    temporary.replace(path)


def start(name, definition, initial=False):
    if name not in ('lab', 'peer') or name in children:
        raise ValueError('duplicate or foreign child')
    if definition['command'] != [daemon, '--instance', name]:
        raise ValueError('unexpected command')
    generation = definition['env'][generation_env]
    if len(generation) != 64 or any(c not in '0123456789abcdef' for c in generation):
        raise ValueError('invalid generation')
    child = subprocess.Popen(definition['command'], executable=str(root / 'controller'),
                             env={generation_env: generation}, stdin=subprocess.PIPE,
                             stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL)
    children[name] = child
    (root / 'proc' / str(child.pid)).symlink_to(f'/proc/{child.pid}')
    definitions[name] = dict(definition, running=True, pid=child.pid)
    status(name, 'ERROR' if mode == 'startup-error' and not initial else 'RUNNING')


def dispatch(args):
    if len(args) != 4 or args[0] != 'call':
        raise ValueError('unexpected ubus arguments')
    _, service, method, raw = args
    request = json.loads(raw)
    if service == 'fixture' and method == 'seed':
        if children or set(request) != {'lab', 'peer'}:
            raise ValueError('invalid seed')
        for name, generation in request.items():
            start(name, dict(command=[daemon, '--instance', name],
                             env={generation_env: generation},
                             respawn=['3600', '5', '5'], stdout=True, stderr=True), True)
        return 0, {}
    if service == 'fixture' and method == 'repair' and request == {}:
        for name in children:
            status(name, 'RUNNING')
        return 0, {}
    if service != 'service' or request.get('name') != 'cake-autorate':
        raise ValueError('foreign service')
    if method == 'list':
        return 0, {'cake-autorate': {'instances': definitions}}
    if method == 'delete':
        if request != {'name': 'cake-autorate', 'instance': 'lab'}:
            raise ValueError('broad or peer delete')
        child = children['lab']
        child.stdin.close()
        child.wait(timeout=5)
        del children['lab']
        (root / 'proc' / str(child.pid)).unlink()
        del definitions['lab']
        del statuses['lab']
        actions.append('delete:lab')
    elif method == 'add':
        if (set(request) != {'name', 'script', 'instances'}
                or request['script'] != '/etc/init.d/cake-autorate'
                or set(request['instances']) != {'lab'}):
            raise ValueError('broad or peer add')
        start('lab', request['instances']['lab'])
        actions.append('add:lab')
    else:
        raise ValueError('unexpected method')
    (root / 'actions.json').write_text(json.dumps(actions))
    return (1 if method == 'add' and mode == 'lost-add-ack' else 0), {}


with socket.socket(socket.AF_UNIX) as server:
    server.bind(endpoint)
    server.listen(4)
    server.settimeout(0.2)
    stopped = threading.Event()

    def serve():
        while not stopped.is_set():
            # Dummy controllers have no I/O; the model publishes their periodic
            # status atomically, preserving process start time and ERROR state.
            for name in children:
                status(name, statuses[name]['state'])
            try:
                client, _ = server.accept()
            except socket.timeout:
                continue
            with client:
                client.settimeout(5)
                try:
                    with client.makefile('rb') as stream:
                        args = json.loads(stream.readline(65536))
                    code, body = dispatch(args)
                except (ValueError, KeyError, OSError, subprocess.TimeoutExpired) as error:
                    print('fixture request rejected:', str(error), file=sys.stderr, flush=True)
                    code, body = 92, {}
                client.sendall(json.dumps(dict(exit=code, body=body)).encode() + b'\n')

    worker = threading.Thread(target=serve)
    worker.start()
    try:
        result = subprocess.run([sys.argv[3], '--exact', sys.argv[4], '--nocapture',
                                 '--test-threads=1'], timeout=60)
        code = result.returncode
    finally:
        stopped.set()
        worker.join(timeout=6)
        for child in children.values():
            if not child.stdin.closed:
                child.stdin.close()
            try:
                child.wait(timeout=5)
            except subprocess.TimeoutExpired:
                child.kill()  # Only a child retained in this supervisor's map.
                child.wait()
    raise SystemExit(code)
