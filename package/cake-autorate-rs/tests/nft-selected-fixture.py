"""Hermetic nft command model for selected-classifier tests; no kernel calls."""
import copy
import json
from pathlib import Path
import re
import sys

root = Path(sys.argv[1])
if not root.is_absolute() or (root / 'fixture.owner').read_text() != 'cake-nft-selected-fixture-v1\n':
    raise SystemExit(90)
args = sys.argv[2:]
table = 'cake_autorate_dscp'
path = root / 'kernel.json'
kernel = json.loads(path.read_text()) if path.exists() else None

if args == ['list', 'table', 'inet', table]:
    raise SystemExit(0 if kernel is not None else 1)
if args == ['-j', 'list', 'table', 'inet', table]:
    if kernel is None:
        raise SystemExit(1)
    print(json.dumps(kernel, sort_keys=True, separators=(',', ':')))
    raise SystemExit(0)

dry = args == ['-c', '-f', '-']
restoring = args == ['-j', '-f', '-']
deleting = args == ['delete', 'table', 'inet', table]
if not (dry or restoring or deleting or args == ['-f', '-']):
    raise SystemExit(91)
text = '' if deleting else sys.stdin.read()
current = copy.deepcopy(kernel)
counter_path = root / 'counter'
counter = int(counter_path.read_text()) if counter_path.exists() else 10000


def handle():
    global counter
    counter += 1
    return counter


def remove_rule(chain, number):
    if current is None:
        raise ValueError('missing table')
    matches = [item for item in current['nftables']
               if item.get('rule', {}).get('chain') == chain
               and item.get('rule', {}).get('handle') == number]
    if len(matches) != 1:
        raise ValueError('missing handle')
    current['nftables'].remove(matches[0])


def add_rule(rule):
    if current is None or rule['family'] != 'inet' or rule['table'] != table:
        raise ValueError('wrong table')
    if rule['chain'] not in ('forward', 'output'):
        raise ValueError('wrong chain')
    rule = copy.deepcopy(rule)
    rule['handle'] = handle()
    current['nftables'].append({'rule': rule})


try:
    if deleting:
        if current is None:
            raise ValueError('missing table')
        current = None
    elif restoring:
        for operation in json.loads(text)['nftables']:
            if 'delete' in operation:
                rule = operation['delete']['rule']
                if rule['family'] != 'inet' or rule['table'] != table:
                    raise ValueError('wrong deletion scope')
                remove_rule(rule['chain'], rule['handle'])
            elif 'add' in operation:
                add_rule(operation['add']['rule'])
            else:
                raise ValueError('unsupported restore')
    else:
        for line in text.splitlines():
            created = re.fullmatch(r'create table inet ' + table + r' \{ comment "([a-z0-9-]+)"; \}', line)
            if created:
                if current is not None:
                    raise ValueError('table already exists')
                current = {'nftables': [{'metainfo': {'json_schema_version': 1}},
                                       {'table': {'family': 'inet', 'name': table,
                                                  'handle': handle(), 'comment': created[1]}}]}
                continue
            chain = re.fullmatch(r'add chain inet ' + table + r' (forward|output) \{ type (filter|route) hook (forward|output) priority -140; policy accept; \}', line)
            if chain:
                if current is None:
                    raise ValueError('missing table')
                current['nftables'].append({'chain': {'family': 'inet', 'table': table,
                    'name': chain[1], 'type': chain[2], 'hook': chain[3], 'prio': -140,
                    'policy': 'accept', 'handle': handle()}})
                continue
            deleted = re.fullmatch(r'delete rule inet ' + table + r' (forward|output) handle ([0-9]+)', line)
            if deleted:
                remove_rule(deleted[1], int(deleted[2]))
                continue
            added = re.fullmatch(r'add rule inet ' + table + r' (forward|output) (oifname "([a-zA-Z0-9_.:@-]+)" .+)', line)
            if added:
                add_rule({'family': 'inet', 'table': table, 'chain': added[1], 'expr': [
                    {'match': {'op': '==', 'left': {'meta': {'key': 'oifname'}}, 'right': added[3]}},
                    {'fixture_body': added[2]}]})
                continue
            raise ValueError('unsupported statement')
except (ValueError, KeyError, TypeError):
    raise SystemExit(92)

if not dry:
    if (root / 'peer-drift-after-apply').exists() and not restoring:
        (root / 'peer-drift-after-apply').unlink()
        for item in current['nftables']:
            rule = item.get('rule')
            if rule and rule['expr'][0]['match']['right'] == 'eth1':
                rule['expr'].append({'fixture_peer_drift': True})
                break
    if current is None:
        path.unlink(missing_ok=True)
    else:
        path.write_text(json.dumps(current, sort_keys=True, separators=(',', ':')))
    counter_path.write_text(str(counter))
    with (root / 'actions.jsonl').open('a') as stream:
        stream.write(json.dumps({'args': args, 'input': text}) + '\n')
    if (root / 'fail-after-apply').exists() and not restoring:
        (root / 'fail-after-apply').unlink()
        raise SystemExit(93)
