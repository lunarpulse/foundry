#!/usr/bin/env python3
"""Independent, stdlib-only declarative pack verifier; never executes lens prompts.
Oracle: bmad-build step-04 cascading contract. No engine/test code is imported.
Models successful dispatch/redrive; holds stop execution before human resume.
The 60 cases are 15 NONEMPTY category subsets x two fix x two loop counts.
"""
import argparse
import copy
import hashlib
import itertools
from pathlib import Path, PurePosixPath
import re
import struct
import sys
import tomllib


class Expr:
    """Recursive descent parser; consumes all tokens (no eval or short-circuit parsing)."""
    token = re.compile(r'\s*(\&\&|\|\||[!(),\[\]]|"(?:[^"\\]|\\.)*"|[a-z_][a-z0-9_]*|[0-9]+)')

    def __init__(self, text, state):
        self.tokens = []
        pos = 0
        while pos < len(text.rstrip()):
            m = self.token.match(text, pos)
            if not m:
                raise ValueError(f"bad expression at {text[pos:]!r}")
            self.tokens.append(m[1])
            pos = m.end()
        self.i, self.state = 0, state

    def take(self, expected=None):
        if self.i == len(self.tokens):
            raise ValueError('unexpected end of expression')
        t = self.tokens[self.i]
        self.i += 1
        if expected is not None and t != expected:
            raise ValueError(f'expected {expected}, got {t}')
        return t

    def peek(self, token):
        return self.i < len(self.tokens) and self.tokens[self.i] == token

    def run(self):
        result = self.disjunction()
        if self.i != len(self.tokens):
            raise ValueError('trailing expression tokens')
        return result

    def disjunction(self):
        value = self.conjunction()
        while self.peek('||'):
            self.take()
            rhs = self.conjunction()
            value = value or rhs
        return value

    def conjunction(self):
        value = self.unary()
        while self.peek('&&'):
            self.take()
            rhs = self.unary()
            value = value and rhs
        return value

    def unary(self):
        if self.peek('!'):
            self.take()
            return not self.unary()
        if self.peek('('):
            self.take()
            value = self.disjunction()
            self.take(')')
            return value
        name = self.take()
        if name in ('true', 'false'):
            return name == 'true'
        self.take('(')
        if name == 'any_in':
            self.take('verdicts')
            self.take(',')
            self.take('[')
            values = []
            if not self.peek(']'):
                while True:
                    literal = self.take()
                    if not literal.startswith('"'):
                        raise ValueError('expected string literal')
                    # (F5-3) normative: TOML owns decoding — the DSL
                    # compares the string CONTENT as-is. Strip quotes
                    # without a second escape decode.
                    inner = literal[1:-1]
                    if '\\' in inner:
                        raise ValueError(
                            'escape sequences are TOML-owned; DSL '
                            'literals arrive pre-decoded')
                    values.append(inner)
                    if not self.peek(','):
                        break
                    self.take(',')
            self.take(']')
            value = bool(set(values) & self.state['verdicts'])
        elif name == 'count_lt':
            counter = self.take()
            self.take(',')
            bound = self.take()
            if not bound.isdecimal() or int(bound) >= 2**64:
                raise ValueError('invalid u64')
            value = self.state['counters'][counter] < int(bound)
        elif name == 'has_findings':
            group = self.take()
            # (F5-3) §2.3: exactly the literal token 'any' — quoted
            # strings and arbitrary group ids are syntax errors
            if group != 'any':
                raise ValueError(f'has_findings requires (any), got {group!r}')
            active = self.state['verdicts'] & self.state['active']
            value = bool(active)
        elif name in ('budget_exhausted', 'timeout'):
            value = self.state[name]
        else:
            raise ValueError('unsupported function: ' + name)
        self.take(')')
        return value


def state(categories=(), fix=0, loops=0, active=None):
    return dict(verdicts=set(categories), counters=dict(fix_rounds=fix, loopbacks=loops),
                active=set(active if active is not None else ('intent_gap', 'bad_plan', 'patch')),
                budget_exhausted=False, timeout=False, reverted=False, events=[])


def actions(text, s):
    """(r30-3/F10-1) full-normative parser; returns parsed action list."""
    if text is None:
        return []
    parts = text.split(';')
    if any(not p.strip() for p in parts):
        raise ValueError('empty action in sequence: ' + repr(text))
    out = []
    for action in map(str.strip, parts):
        m = re.fullmatch(r'(bump|revert)\s*\(\s*([a-z_][a-z0-9_]*)\s*\)', action)
        if m:
            op, arg = m.groups()
            if op == 'bump':
                s['counters'][arg] = s['counters'].get(arg, 0) + 1
            elif arg in ('origin', 'latest'):
                s['reverted'] = True
            else:
                raise ValueError('unsupported revert base: ' + arg)
            s['events'].append(f'{op}({arg})')
            out.append((op, arg))
            continue
        m = re.fullmatch(r'emit\s*\(\s*([A-Za-z_]\w*)\s*,\s*([A-Za-z_]\w*)\s*\)', action)
        if m:
            s['events'].append(f'emit({m.group(1)}, {m.group(2)})')
            out.append(('emit', m.group(1)))
            continue
        raise ValueError('unsupported action: ' + action)
    return out


def decision(policy, s, start=None):
    """Walk actual graph to first hold/emit/redrive; actions occur before entry."""
    nodes = {n['id']: n for n in policy['nodes']}
    cursor = start or policy['entry']
    for _ in range(100):
        n = nodes[cursor]
        kind = n['kind']
        s['events'].append(f'enter:{cursor}')
        if kind in ('hold', 'emit', 'redrive'):
            return n
        if kind == 'dispatch':
            cursor = n['on_done']  # supplied normalized successful triage output
        elif kind == 'route':
            for branch in n['branches']:
                if Expr(branch['when'], s).run():
                    actions(branch.get('do'), s)
                    cursor = branch['goto']
                    break
            else:
                raise ValueError('no route branch matched')
        else:
            raise ValueError('unknown node kind')
    raise ValueError('nonterminating first decision')


def oracle(categories, fix, loops):
    # Written from original contract, independent of policy branch expressions.
    if set(categories) & {'intent_gap', 'bad_plan'}:
        loops += 1
        target = 'human' if loops > 5 else ('human_gap' if 'intent_gap' in categories else 'replan')
        return target, loops
    return ('fixdie' if fix < 3 else 'human') if 'patch' in categories else 'pass', loops


def canonical(root):
    raw = (root / 'manifest.toml').read_bytes()
    manifest = tomllib.loads(raw.decode())
    files = manifest['files']
    assert len(files) == len(set(files)), 'duplicate files'
    # (r31-2) execution binding: the EXECUTED policy must be a member
    assert 'policy.toml' in files, \
        'manifest must declare policy.toml (execution binding)'
    assert 'manifest.toml' not in files, 'manifest listed twice'
    refs = set(files)
    for name in files:
        p = PurePosixPath(name)
        assert not p.is_absolute() and '..' not in p.parts and str(p) == name, f'unsafe path {name}'
        assert (root / name).resolve().is_relative_to(root.resolve()), f'escaping path {name}'
        assert (root / name).is_file(), f'missing file {name}'
    digest = hashlib.sha256()
    for name in sorted(refs | {'manifest.toml'}, key=lambda p: p.encode('utf-8')):
        data = (root / name).read_bytes()
        assert b'\r' not in data and data.endswith(b'\n'), f'noncanonical newlines: {name}'
        if name == 'manifest.toml':
            lines = data.splitlines(keepends=True)
            indices = [i for i, line in enumerate(lines) if re.match(rb'^\s*digest =', line)]
            assert len(indices) == 1, 'expected exactly one digest line'
            assert not any(line.lstrip().startswith(b'[') for line in lines[:indices[0]]), 'digest not top-level'
            data = b''.join(line for i, line in enumerate(lines) if i != indices[0])
        path = name.encode('utf-8')
        digest.update(struct.pack('<Q', len(path)) + path + struct.pack('<Q', len(data)) + data)
    # All declarative file references must belong to the manifest closure.
    def walk(value):
        if isinstance(value, dict):
            for key, child in value.items():
                if key == 'lenses':
                    assert set(child) <= refs, f'unlisted lens {child}'
                if key in ('output_schema', 'schema'):
                    assert child in refs, f'unlisted schema {child}'
                walk(child)
        elif isinstance(value, list):
            for child in value:
                walk(child)
    for name in files:
        if name.endswith('.toml'):
            walk(tomllib.loads((root / name).read_text()))
    actual = digest.hexdigest()
    assert actual == manifest['digest'], f'digest declared={manifest["digest"]} calculated={actual}'
    return f'version={manifest["version"]}, files={len(files)}+manifest, sha256={actual}'


EV_DECLARED = set()  # filled by main() from schema outputs after load


def graph(policy):
    nodes = {n['id']: n for n in policy['nodes']}
    assert len(nodes) == len(policy['nodes']), 'duplicate node IDs'
    assert policy['entry'] in nodes, 'missing entry'
    stages = {s['id'] for s in policy.get('pack_stages', [])}
    # (F2-1) general load validation: error policy enums, emit
    # payload_from, timeout_s, counter u64, closed role set
    for n in nodes.values():
        if n['kind'] == 'emit':
            assert n.get('payload_from'), \
                f'emit {n["id"]} missing payload_from'
            # (F11-1) closed event set (canonical pair allowed)
            ev_ = n.get('event')
            assert ev_ in ('InspectionPassed', 'InspectionFailed', 'summary') \
                or ev_ in EV_DECLARED, \
                f'emit {n["id"]} event {ev_!r} not in the closed event set'
        if n['kind'] == 'dispatch':
            # (F11-2) >=1 stage per dispatch
            assert n.get('stages'), \
                f'dispatch {n["id"]} must declare at least one stage'
        if n['kind'] == 'hold' and 'timeout_s' in n:
            t = n['timeout_s']
            assert isinstance(t, int) and not isinstance(t, bool) and t > 0, \
                f'hold {n["id"]} timeout_s must be a positive int'
        for f in ('on_verify_invalid', 'on_no_valid', 'on_redrive_error'):
            if f in n:
                assert n[f] in ('hold', 'fail-run'), \
                    f'{n["id"]}.{f}={n[f]!r} invalid (hold|fail-run — §2.3)'
        for st_ in n.get('stages', []) or []:
            if 'role' in st_:
                assert st_['role'] in ('lens', 'verify', 'replan'), \
                    f'stage {st_["id"]} role {st_["role"]!r} not in lens|verify|replan'
            # (F4-2) every stage lists >=1 lens (§A.4)
            ls_ = st_.get('lenses')
            assert isinstance(ls_, list) and len(ls_) >= 1, \
                f'stage {st_["id"]} must list at least one lens'
        # (F3-2) pack_stages roles share the closed set
        for ps_ in policy.get('pack_stages', []) or []:
            if 'role' in ps_:
                assert ps_['role'] in ('lens', 'verify', 'replan'), \
                    f'pack_stage {ps_.get("id")} role {ps_["role"]!r} invalid'
            # (F5-2) registered stages list >=1 lens
            pls_ = ps_.get('lenses')
            assert isinstance(pls_, list) and len(pls_) >= 1, \
                f'pack_stage {ps_.get("id")} must list at least one lens'
    # (F5-1/F6-2) §2.5 on_error completeness + state→policy mapping
    NS = {'ok', 'dispatch_error', 'parse_error', 'timeout'}
    NP = {'accept-stage', 'discard-stage', 'discard-finding', 'hold'}
    SP = {'ok': 'accept-stage', 'dispatch_error': 'discard-stage',
          'parse_error': 'discard-finding', 'timeout': 'hold'}
    declared = set()
    for oe_ in policy.get('output', {}).get('on_error', []) or []:
        assert oe_.get('state') in NS, f'on_error state {oe_.get("state")!r} not normative'
        # (F7-4) per-state ALLOWED sets — pack owns the choice
        SP_ALLOWED = {'ok': {'accept-stage'},
                      'dispatch_error': {'discard-stage', 'fail-run'},
                      'parse_error': {'discard-finding', 'fail-run'},
                      'timeout': {'hold', 'fail-run'}}
        assert oe_.get('policy') in SP_ALLOWED[oe_['state']], \
            f'on_error {oe_["state"]} policy {oe_.get("policy")!r} not in {SP_ALLOWED[oe_["state"]]} (§2.5)'
        declared.add(oe_['state'])
    assert NS <= declared, f'on_error missing: {NS - declared}'
    # (F6-1) verify<=1 per pack; stage IDs unique across BOTH node
    # stages and pack_stages; verify stages declare after deps
    sid_list = []
    nverify = 0
    for n_ in nodes.values():
        for st_ in n_.get('stages', []) or []:
            sid_list.append(st_['id'])
            if st_.get('role') == 'verify':
                nverify += 1
                assert st_.get('after'), \
                    f'verify stage {st_["id"]} must declare after dependencies'
                lens_ids_ = [s['id'] for s in n_.get('stages', [])
                             if s.get('role') == 'lens']
                miss_ = set(lens_ids_) - set(st_['after'])
                assert not miss_, \
                    f'verify stage {st_["id"]} must run after ALL lens stages; missing: {sorted(miss_)}'
    for ps_ in policy.get('pack_stages', []) or []:
        sid_list.append(ps_.get('id'))
        if ps_.get('role') == 'verify':
            nverify += 1
    assert nverify <= 1, f'pack declares {nverify} verify stages (max 1, §2.3)'
    dupes_ = {x for x in sid_list if sid_list.count(x) > 1}
    assert not dupes_, f'duplicate stage IDs: {sorted(dupes_)}'
    # (F8-1/F9-1) registered-stage deps: defined + ACYCLIC over the
    # full stage dependency graph (node stages + pack_stages)
    sset_ = set(sid_list)
    dep_adj_ = {}
    for ps_ in policy.get('pack_stages', []) or []:
        deps_ = ps_.get('after', []) or []
        for dep_ in deps_:
            assert dep_ in sset_, f'pack_stage {ps_.get("id")} depends on undefined {dep_!r}'
        dep_adj_[ps_.get('id')] = list(deps_)
    for n_ in nodes.values():
        for st_ in n_.get('stages', []) or []:
            for dep_ in st_.get('after', []) or []:
                dep_adj_.setdefault(st_['id'], []).append(dep_)
    _mk = {}
    def _dfsc(u):
        _mk[u] = 1
        for w in dep_adj_.get(u, ()):
            if _mk.get(w) == 1:
                return False
            if _mk.get(w, 0) == 0 and not _dfsc(w):
                return False
        _mk[u] = 2
        return True
    for v_ in dep_adj_:
        if _mk.get(v_, 0) == 0:
            assert _dfsc(v_), f'stage dependency cycle involving {v_!r}'
    # (F8-2) resume/route edges must form a DAG after removing deduct
    non_deduct_ = {i for i, n in nodes.items()
                   if n['kind'] not in ('dispatch', 'redrive')}
    adj_ = {}
    for i, n in nodes.items():
        if i not in non_deduct_:
            continue
        outs = []
        if n['kind'] == 'route':
            outs = [b['goto'] for b in n['branches'] if b.get('goto')]
        elif n['kind'] == 'hold':
            outs = list(n.get('resume_targets', []))
        for o in outs:
            if o in non_deduct_:
                adj_.setdefault(i, set()).add(o)
    state_ = {}
    def dfs_(u):
        state_[u] = 1
        for w in adj_.get(u, ()):
            if state_.get(w) == 1:
                return False
            if state_.get(w, 0) == 0 and not dfs_(w):
                return False
        state_[u] = 2
        return True
    for v_ in non_deduct_:
        if state_.get(v_, 0) == 0:
            assert dfs_(v_), 'hold/route graph has a cycle after deduct removal (§2.4b)'
    # (F3-3) emit payload_from validated after schemas load (main)
    # (F3-1/F4-1) top-level [hold] table is MANDATORY
    assert 'hold' in policy, 'pack must declare a top-level [hold] table'
    ht = policy['hold'].get('timeout_s')
    assert ht is not None, 'top-level [hold] must declare timeout_s (0=infinite)'
    assert isinstance(ht, int) and not isinstance(ht, bool) and ht >= 0, \
        'top-level [hold] timeout_s must be >= 0 (0=infinite)'
    for cname, cval in policy.get('counters', {}).items():
        assert isinstance(cval, int) and not isinstance(cval, bool) \
            and 0 <= cval < 2**64, \
            f'counter {cname} initial value must be a u64 int'
    # (r32-1) mandatory fields + §2.4b resume-cycle on every pack
    deduct = {n['id'] for n in nodes.values()
              if n['kind'] in ('dispatch', 'redrive')}
    for n in nodes.values():
        kind = n['kind']
        assert kind in ('dispatch', 'route', 'redrive', 'emit', 'hold'), 'unknown node kind'
        if kind == 'dispatch':
            for f in ('on_no_valid', 'on_verify_invalid', 'hold_target'):
                assert f in n, f'dispatch {n["id"]} missing {f}'
        if kind == 'hold':
            assert n.get('resume_targets') is not None, \
                f'hold {n["id"]} missing resume_targets'
        refs = [n[k] for k in ('on_done', 'hold_target') if k in n]
        if kind == 'route':
            assert 'on_done' not in n, f'route {n["id"]} must not declare on_done (§2.3)'
            assert n['branches'][-1]['when'].strip() == 'true', 'missing last true fallback'
            for b in n['branches']:
                Expr(b['when'], state()).run()
                acts_ = actions(b.get('do'), state())
                # (F11-3) bump targets must be DECLARED — dead branches too
                for a_ in acts_:
                    if a_[0] == 'bump':
                        assert a_[1] in policy.get('counters', {}), \
                            f'branch bumps undeclared counter {a_[1]!r} (route {n["id"]})'
                # (F10-1) emit is terminal: nothing after, no goto
                emits_ = [i for i, a in enumerate(acts_) if a[0] == 'emit']
                for i_ in emits_:
                    assert i_ == len(acts_) - 1, \
                        f'route {n["id"]} branch has action after emit'
                    assert not b.get('goto'), \
                        f'route {n["id"]} branch emits AND gotos (§2.3)'
                assert len(emits_) <= 1, f'route {n["id"]} branch multi-emit (§2.3)'
                refs.append(b['goto'])
        if kind == 'hold':
            refs.extend(n['resume_targets'])
            assert 'on_done' not in n, 'hold has on_done'
        if kind == 'emit':
            assert not any(k in n for k in ('on_done', 'goto', 'hold_target', 'resume_targets', 'branches')), 'emit not terminal'
        if 'pre_stage' in n:
            assert n['pre_stage'] in stages, 'unknown pre_stage'
        assert all(r in nodes for r in refs), f'dangling node reference from {n["id"]}'
    return f'{len(nodes)} nodes, entry/references/fallback/resume/emit valid'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('pack', nargs='?', type=Path, default=Path('skills/bmad-review-full/v1'))
    args = parser.parse_args()
    failures = []

    def check(label, fn):
        try:
            detail = fn()
            print(f'PASS {label}' + (f': {detail}' if detail else ''))
        except (AssertionError, ValueError, KeyError, OSError, TypeError) as e:
            failures.append(label)
            print(f'FAIL {label}: {e}')

    check('manifest closure + canonical digest', lambda: canonical(args.pack))
    policy = tomllib.loads((args.pack / 'policy.toml').read_text())
    # (r17-4) follow the ACTUAL verify stage's output_schema reference
    # instead of hardcoding schema-triage.toml
    def _verify_schema_path(pol):
        for n in pol['nodes']:
            if n.get('kind') == 'dispatch':
                for st in n.get('stages', []):
                    if st.get('role') == 'verify':
                        return st['output_schema']
        raise AssertionError('no verify stage with output_schema')
    schema_name = _verify_schema_path(policy)
    schema = tomllib.loads((args.pack / schema_name).read_text())
    # negative control: swapping the reference must break this lookup
    swapped = tomllib.loads((args.pack / 'policy.toml').read_text())
    for n in swapped['nodes']:
        if n.get('kind') == 'dispatch':
            for st in n.get('stages', []):
                if st.get('role') == 'verify':
                    st['output_schema'] = 'schema-findings.toml'
    try:
        sname2 = _verify_schema_path(swapped)
        s2 = tomllib.loads((args.pack / sname2).read_text())
        next(o['active_categories'] for o in s2['output'] if o['name'] == 'verdicts')
        raise AssertionError('schema swap mutant NOT detected')
    except StopIteration:
        pass  # expected: findings schema has no verdicts output
    active = next(o['active_categories'] for o in schema['output'] if o['name'] == 'verdicts')
    # (F3-3) emit payload_from must reference a DECLARED output across
    # ALL schema files in the pack (or the reserved 'summary')
    _files = tomllib.loads((args.pack / 'manifest.toml').read_text())['files']
    declared_out = {o['name'] for s in
                    [tomllib.loads((args.pack / f).read_text())
                     for f in _files if f.startswith('schema-')]
                    for o in s.get('output', [])}
    # (F11-1) schema outputs double as the declared event set
    global EV_DECLARED
    EV_DECLARED = set(declared_out)
    for n in policy['nodes']:
        if n.get('kind') == 'emit':
            pf = n.get('payload_from')
            assert pf, f'emit {n["id"]} missing payload_from'
            assert pf in declared_out or pf == 'summary', \
                f'emit {n["id"]} payload_from={pf!r} not a declared output'
    check('graph structure', lambda: graph(policy))

    def expression_tests():
        s = state(['patch'], fix=2, loops=5)
        cases = [('false || true && false', False), ('!false && (true || false)', True),
                 ('any_in(verdicts, ["bad_plan", "patch"])', True),
                 ('count_lt(fix_rounds, 3) && !count_lt(loopbacks, 5)', True),
                 ('has_findings(any)', True), ('budget_exhausted() || timeout()', False)]
        for expr, expected in cases:
            assert Expr(expr, s).run() == expected, expr
        for flag in ('timeout', 'budget_exhausted'):
            s[flag] = True
            assert Expr(flag + '()', s).run()
            s[flag] = False
        for invalid in ('true garbage', 'true ||', 'evil()', 'count_lt(fix_rounds, -1)'):
            try:
                Expr(invalid, s).run()
            except ValueError:
                continue
            raise AssertionError('accepted invalid expression ' + invalid)
        return 'precedence, parentheses, functions, flags, malformed input'
    check('independent expression interpreter', expression_tests)
    categories = ('intent_gap', 'bad_plan', 'patch', 'defer')
    subsets = [c for size in range(1, 5) for c in itertools.combinations(categories, size)]
    route_matches = state_matches = total = 0
    for cats, fix, loops in itertools.product(subsets, (2, 3), (4, 5)):
        total += 1
        s = state(cats, fix, loops, active)
        node = decision(policy, s)
        expected_node, expected_count = oracle(cats, fix, loops)
        route_matches += node['id'] == expected_node
        state_matches += node['id'] == expected_node and s['counters']['loopbacks'] == expected_count
        def verify_case():
            assert (node['id'], s['counters']['loopbacks']) == (expected_node, expected_count), (
                f'actual={node["id"]}/loopbacks={s["counters"]["loopbacks"]}, '
                f'expected={expected_node}/loopbacks={expected_count}')
        check(f'case {total:02d} {"+".join(cats)} fix={fix} loops={loops}', verify_case)
    print(f'MATRIX destinations={route_matches}/{total}; destination+counter={state_matches}/{total}')

    def repeated():
        s = state(['bad_plan'], active=active)
        trace = []
        for visit in range(1, 8):
            node = decision(policy, s)
            trace.append(f'{visit}:{node["id"]}(loops={s["counters"]["loopbacks"]})')
            if node['kind'] == 'hold':
                print('TRACE bad_plan ' + ' -> '.join(trace))
                assert visit == 6 and node['id'] == 'human', 'wrong halt visit/destination'
                assert s['counters']['loopbacks'] == 6, 'sixth entry must persist increment to 6 before HALT; actual=5'
                return
            assert node['kind'] == 'redrive', 'unexpected terminal'
            if 'bump' in node:
                actions(f'bump({node["bump"]})', s)
        raise AssertionError('no halt')
    check('repeated bad_plan sixth-entry state', repeated)

    def gap_timing(p=policy):
        s = state(['intent_gap', 'bad_plan', 'patch', 'defer'], loops=4, active=active)
        n = decision(p, s)
        assert n['kind'] == 'hold' and n['id'] == 'human_gap', 'wrong gap hold'
        assert s['counters']['loopbacks'] == 5 and s['reverted'], 'hold entered before bump/revert'
        e = s['events']
        assert e.index('bump(loopbacks)') < e.index('revert(origin)') < e.index('enter:human_gap'), e
        return ' -> '.join(e)
    check('intent_gap pre-hold timing', gap_timing)

    def declarations():
        ns = {n['id']: n for n in policy['nodes']}
        assert ns['human_gap']['resume_targets'] == ['replan_full']
        for name, payload in [('replan', 'plan'), ('replan_full', 'plan_full')]:
            assert ns[name]['base'] == 'origin' and ns[name]['payload'] == payload
            assert ns[name]['on_done'] == policy['entry'] and 'pre_stage' in ns[name]
        assert ns['fixdie']['target'] == 'die-workdir' and ns['fixdie']['base'] == 'latest'
        assert ns['fixdie']['payload'] == 'findings' and ns['fixdie']['bump'] == 'fix_rounds'
        assert 'deferred' in ns[policy['entry']]['side_records']
        deferred = next(o for o in schema['output'] if o['name'] == 'deferred')
        for cats in subsets:
            expected = not bool(set(cats) & {'intent_gap', 'bad_plan'})
            assert Expr(deferred['when'], state(cats)).run() == expected, 'defer moot mismatch'
        return 'gap resume/full plan, bad_plan origin, patch continuation declarations, defer guard'
    check('continuation + defer declarations', declarations)
    check('empty set supplementary', lambda: assert_empty(policy, active))
    for flag in ('budget_exhausted', 'timeout'):
        def emergency():
            s = state(['intent_gap'], active=active)
            s[flag] = True
            assert decision(policy, s)['id'] == 'human'
        check('emergency ' + flag, emergency)

    # In-memory mutations isolate semantic detectors from digest mismatch.
    def negative_timing():
        mutant = copy.deepcopy(policy)
        route = next(n for n in mutant['nodes'] if n['id'] == 'route1')
        b = next(b for b in route['branches'] if b.get('goto') == 'human_gap')
        del b['do']
        try:
            gap_timing(mutant)
        except AssertionError as e:
            return 'mutant rejected: ' + str(e)
        raise AssertionError('bad pack accepted')
    check('negative control remove gap pre-hold actions', negative_timing)

    def negative_graph():
        mutant = copy.deepcopy(policy)
        next(n for n in mutant['nodes'] if n['kind'] == 'hold')['resume_targets'] = ['missing']
        try:
            graph(mutant)
        except AssertionError as e:
            return 'mutant rejected: ' + str(e)
        raise AssertionError('bad graph accepted')
    check('negative control dangling resume', negative_graph)
    print('SCOPE: declarative model; no live engine/subagent execution. Prompt compliance and actual session reuse are not proven.')
    print(f'RESULT: {"FAIL" if failures else "PASS"}; {len(failures)} failed checks')
    return bool(failures)


def assert_empty(policy, active):
    assert decision(policy, state(active=active))['id'] == 'pass'


if __name__ == '__main__':
    sys.exit(main())
