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
            # (F29-4) emit payload must be a declared output — enforced
            # when the state declares its output universe; the walker
            # state (no 'outputs' key) is load-checked in graph()
            if 'outputs' in s:
                assert m.group(2) in s['outputs'], \
                    f'emit({m.group(1)}, {m.group(2)}): payload not a declared output'
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



def _outs19(n):
    k = n['kind']
    if k == 'route':
        return [b.get('goto') for b in n['branches'] if b.get('goto')]
    if k in ('dispatch', 'redrive'):
        return [n['on_done']] if n.get('on_done') else []
    if k == 'hold':
        return list(n.get('resume_targets', []))
    return []






def _guard_tree_a(py):
    import re as _re
    toks = _re.findall(
        r'!|&&|\|\||\(|\)|,|\[|\]|"[^"]*"|\d+\.\d+|\d+'
        r'|[A-Za-z_][A-Za-z0-9_]*', py)
    pos = [0]
    def peek():
        return toks[pos[0]] if pos[0] < len(toks) else None
    def eat(t=None):
        tk = toks[pos[0]] if pos[0] < len(toks) else None
        if tk is None or (t is not None and tk != t):
            raise ValueError(f'guard syntax: {py!r} at {tk!r}')
        pos[0] += 1
        return tk
    def atom():
        tk = peek()
        if tk == '(':
            eat('(')
            v = orx()
            eat(')')
            return v
        if tk == '[':
            eat('[')
            items = []
            while peek() != ']':
                items.append(eat())
                if peek() == ',':
                    eat(',')
            eat(']')
            return ('list', items)
        if tk in ('true', 'false'):
            eat()
            return ('const', tk == 'true')
        if tk and _re.fullmatch(r'\d+(\.\d+)?', tk):
            return ('num', eat())
        if tk and _re.fullmatch(r'[A-Za-z_][A-Za-z0-9_]*', tk):
            name = eat()
            if peek() == '(':
                eat('(')
                args = []
                while peek() != ')':
                    args.append(atom())
                    if peek() == ',':
                        eat(',')
                eat(')')
                return ('call', name, args)
            return ('ident', name)
        raise ValueError(f'guard syntax: {py!r} at {tk!r}')
    def unary():
        if peek() == '!':
            eat('!')
            return ('not', unary())
        return atom()
    def andx():
        v = unary()
        while peek() == '&&':
            eat('&&')
            v = ('and', [v, unary()])
        return v
    def orx():
        v = andx()
        while peek() == '||':
            eat('||')
            v = ('or', [v, andx()])
        return v
    t = orx()
    if pos[0] != len(toks):
        raise ValueError(f'guard trailing syntax: {py!r}')
    return t


def _tree_has_cl_a(t):
    if not isinstance(t, tuple):
        return False
    if t[0] == 'call' and t[1] == 'count_lt':
        return True
    if t[0] in ('and', 'or'):
        return any(_tree_has_cl_a(c) for c in t[1])
    if t[0] == 'not':
        return _tree_has_cl_a(t[1])
    return False


def _tree_const_a(t):
    if not isinstance(t, tuple):
        return None
    if t[0] == 'const':
        return t[1]
    if t[0] == 'not':
        v = _tree_const_a(t[1])
        return None if v is None else (not v)
    if t[0] in ('and', 'or'):
        vals = [_tree_const_a(c) for c in t[1]]
        if any(v is None for v in vals):
            return None
        return all(vals) if t[0] == 'and' else any(vals)
    return None


def _tree_or_weak_a(t):
    if not isinstance(t, tuple):
        return False
    if t[0] == 'or':
        kids = t[1]
        if any(_tree_has_cl_a(k) for k in kids):
            for k in kids:
                if _tree_has_cl_a(k):
                    continue
                if _tree_const_a(k) is False:
                    continue
                return True
        return any(_tree_or_weak_a(k) for k in kids)
    if t[0] in ('and', 'or'):
        return any(_tree_or_weak_a(k) for k in t[1])
    if t[0] == 'not':
        return _tree_or_weak_a(t[1])
    return False


def _tree_neg_cl_a(t):
    def rec(x, neg):
        if not isinstance(x, tuple):
            return False
        if x[0] == 'call' and x[1] == 'count_lt':
            return neg
        if x[0] == 'not':
            return rec(x[1], not neg)
        if x[0] in ('and', 'or'):
            return any(rec(c, neg) for c in x[1])
        return False
    return rec(t, False)

def _reach_set19(adj, start, universe):
    seen_ = set()
    stk_ = [start]
    while stk_:
        x_ = stk_.pop()
        if x_ in seen_ or x_ not in universe:
            continue
        seen_.add(x_)
        for y_ in adj.get(x_, ()):
            if y_ not in seen_:
                stk_.append(y_)
    return seen_

PACK_DIR_ = Path('skills/bmad-review-full/v1')


def _declared_outputs():
    """(F29-4) output names from sibling schema files + 'summary'."""
    names = {'summary'}
    for sf_ in ('schema-triage.toml', 'schema-findings.toml',
                'schema-defer.toml'):
        p_ = PACK_DIR_ / sf_
        if p_.exists():
            names |= {o_.get('name') for o_ in
                      tomllib.loads(p_.read_text()).get('output', [])}
    return names


def graph(policy):
    nodes = {n['id']: n for n in policy['nodes']}
    # (F26-3) entry must be a DISPATCH node (§2.3)
    _ent = nodes.get(policy.get('entry'))
    assert _ent is not None and _ent['kind'] == 'dispatch', \
        f"entry {policy.get('entry')!r} must reference a dispatch node"
    assert len(nodes) == len(policy['nodes']), 'duplicate node IDs'
    assert policy['entry'] in nodes, 'missing entry'
    stages = {s['id'] for s in policy.get('pack_stages', [])}
    # (F2-1) general load validation: error policy enums, emit
    # payload_from, timeout_s, counter u64, closed role set
    for n in nodes.values():
        if n['kind'] == 'emit':
            assert n.get('payload_from'), \
                f'emit {n["id"]} missing payload_from'
            _pf29_ = n['payload_from']
            assert _pf29_ in _declared_outputs(), \
                f'emit {n["id"]} payload_from={_pf29_!r} not a declared output'
            # (F11-1/F12-1) closed event set — canonical pair ONLY
            # (output declarations do NOT extend the event namespace)
            ev_ = n.get('event')
            assert ev_ in ('InspectionPassed', 'InspectionFailed'), \
                f'emit {n["id"]} event {ev_!r} not InspectionPassed|Failed'
        if n['kind'] == 'dispatch':
            # (F11-2) >=1 stage per dispatch
            assert n.get('stages'), \
                f'dispatch {n["id"]} must declare at least one stage'
        if n['kind'] == 'redrive':
            # (F12-2) payload mandatory — unused nodes included
            assert n.get('payload'), \
                f'redrive {n["id"]} missing payload (§2.3)'
            # (F13-2) target mandatory — unused nodes included
            assert n.get('target'), \
                f'redrive {n["id"]} missing target (§2.3)'
            assert n.get('on_done'), \
                f'redrive {n["id"]} missing on_done (§2.3)'
            assert n['on_done'] in nodes, \
                f'redrive {n["id"]} on_done {n["on_done"]!r} dangling'
            assert n.get('on_redrive_error') is not None, \
                f'redrive {n["id"]} missing on_redrive_error (§2.3)'
            assert n.get('hold_target'), \
                f'redrive {n["id"]} missing hold_target (§2.3)'
            assert n['hold_target'] in nodes, \
                f'redrive {n["id"]} hold_target {n["hold_target"]!r} dangling'
            assert nodes[n['hold_target']]['kind'] == 'hold', \
                f'redrive {n["id"]} hold_target must reference a hold node'
            if 'base' in n:
                assert n['base'] in ('origin', 'latest'), \
                    f'redrive {n["id"]} base {n["base"]!r} invalid'
        if n['kind'] != 'hold' and 'timeout_s' in n:
            # (F13-1) timeout_s is a hold-only optional field
            raise AssertionError(
                f'{n["id"]} ({n["kind"]}) declares timeout_s — hold-only')
        if n['kind'] == 'redrive' and n.get('bump'):
            # (F14-1) redrive bump must reference a DECLARED counter
            assert n['bump'] in policy.get('counters', {}),                 f'redrive {n["id"]} bumps undeclared counter {n["bump"]!r}'
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
    # (F29-4) after dependencies must reference DECLARED stage ids
    all_sids_ = {s['id'] for n_ in nodes.values()
                 for s in (n_.get('stages', []) or [])}
    all_sids_ |= {p_.get('id') for p_ in (policy.get('pack_stages', []) or [])}
    for n_ in nodes.values():
        for st_ in n_.get('stages', []) or []:
            _af_ = st_.get('after') or []
            if isinstance(_af_, str):
                _af_ = [_af_]
            for d_ in _af_:
                assert d_ in all_sids_, \
                    f'stage {st_["id"]} after undeclared stage {d_!r}'
    # (F29-4) stages sharing a session must be SERIALIZED: one must
    # explicitly run after the other (parallel same-session forbidden)
    for n_ in nodes.values():
        sst_ = [st_ for st_ in n_.get('stages', []) or [] if st_.get('session')]
        for i_ in range(len(sst_)):
            for j_ in range(i_ + 1, len(sst_)):
                a_, b_ = sst_[i_], sst_[j_]
                if a_['session'] != b_['session']:
                    continue
                _aa_ = a_.get('after') or []
                if isinstance(_aa_, str):
                    _aa_ = [_aa_]
                _ab_ = b_.get('after') or []
                if isinstance(_ab_, str):
                    _ab_ = [_ab_]
                if b_['id'] in _aa_ or a_['id'] in _ab_:
                    continue  # serialized in one direction
                _m_ = ('session %r: %r and %r have no ordering — '
                       'same session must be serial (§2.1)'
                       % (a_['session'], a_['id'], b_['id']))
                raise AssertionError(_m_)
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
        if isinstance(deps_, str):
            deps_ = [deps_]
        for dep_ in deps_:
            assert dep_ in sset_, f'pack_stage {ps_.get("id")} depends on undefined {dep_!r}'
        dep_adj_[ps_.get('id')] = list(deps_)
    for n_ in nodes.values():
        for st_ in n_.get('stages', []) or []:
            af_ = st_.get('after', [])
            if isinstance(af_, str):
                af_ = [af_]
            for dep_ in af_:
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
    # (F22) per-edge cycle validation: hold nodes removed (they cap
    # cycles by human resume), Tarjan SCC, then EVERY cycle-closing
    # edge must be guarded with path-exact bumps
    import re as _re19
    hold_ids = {i_ for i_, n_ in nodes.items() if n_['kind'] == 'hold'}
    auto_adj = {u: [w for w in ws if w not in hold_ids]
                for u, ws in
                {i_: _outs19(nodes[i_]) for i_ in nodes}.items()
                if u not in hold_ids}
    # Tarjan SCC on auto_adj
    idx19, low19, onstk19, stk19, sccs19, ctr19 = {}, {}, {}, [], [], [0]
    def strong19(v):
        idx19[v] = low19[v] = ctr19[0]
        ctr19[0] += 1
        stk19.append(v)
        onstk19[v] = True
        for w in auto_adj.get(v, ()):
            if w not in idx19:
                strong19(w)
                low19[v] = min(low19[v], low19[w])
            elif onstk19.get(w):
                low19[v] = min(low19[v], idx19[w])
        if low19[v] == idx19[v]:
            comp = []
            while True:
                x = stk19.pop()
                onstk19[x] = False
                comp.append(x)
                if x == v:
                    break
            sccs19.append(comp)
    import sys as _s19
    _s19.setrecursionlimit(10000)
    for v19 in list(auto_adj):
        if v19 not in idx19:
            strong19(v19)
    scc_of19 = {}
    for ci19, comp in enumerate(sccs19):
        for u19 in comp:
            scc_of19[u19] = ci19
    # bump aggregation
    bump_ctr = {}
    for n_ in nodes.values():
        bs_ = set()
        if n_['kind'] == 'route':
            for b_ in n_['branches']:
                acts_b = actions(b_.get('do'), {'counters': {}, 'events': []})
                bumps_here = [a_[1] for a_ in acts_b if a_[0] == 'bump']
                for c_ in bumps_here:
                    bs_.add(c_)
                if len(bumps_here) != len(set(bumps_here)):
                    raise AssertionError(f'route {n_["id"]} branch double-bumps a counter')
        elif n_['kind'] == 'redrive' and n_.get('bump'):
            bs_.add(n_['bump'])
        if bs_:
            bump_ctr[n_['id']] = bs_
    # guard extraction + OR/neg checks
    guard_ctrs = {}
    guard_all_ctrs = {}
    for n_ in nodes.values():
        if n_['kind'] != 'route':
            continue
        for b_ in n_['branches']:
            g_ = b_.get('goto')
            w_ = b_.get('when', '')
            ctrs_in_w = _re19.findall(r'count_lt\s*\(\s*([a-z_][a-z0-9_]*)', w_)
            if g_ and ctrs_in_w:
                bi_ = n_['branches'].index(b_)
                guard_ctrs[(n_['id'], g_, bi_)] = ctrs_in_w[0]
                guard_all_ctrs[(n_['id'], g_, bi_)] = set(ctrs_in_w)
                parts_ = _re19.split(r'\|\|', _re19.sub(r'"[^"]*"', '""', w_))
                # (F29-3) structure-aware OR-weakening
                pure_or = _tree_or_weak_a(_guard_tree_a(w_))
                dst_n_ = nodes.get(g_)
                reenters_ = dst_n_ is not None and (
                    dst_n_['kind'] == 'redrive' or g_ == n_['id'])
                neg_entry = reenters_ and _tree_neg_cl_a(_guard_tree_a(w_))
                if pure_or or neg_entry:
                    raise AssertionError(f'guard on {n_["id"]}->{g_} is OR-weakened or negated entry: {w_!r} (§2.2)')
    # (F29-4) route-branch do emit payloads must be declared outputs
    _outs29_ = _declared_outputs()
    for n_ in nodes.values():
        for b_ in n_.get('branches', []) or []:
            _do_ = b_.get('do') or ''
            for _m29_ in re.finditer(
                    r'emit\s*\(\s*\w+\s*,\s*(\w+)\s*\)', _do_):
                assert _m29_.group(1) in _outs29_, \
                    f'{n_["id"]} branch do emit payload ' \
                    f'{_m29_.group(1)!r} not a declared output'
    # (F29-2) every automatic cycle — including route-less redrive
    # chains — must carry a counter bump somewhere on the cycle
    for comp_ in sccs19:
        cnodes_ = set(comp_)
        is_auto_ = (len(cnodes_) > 1) or any(
            x_ in auto_adj.get(x_, ()) for x_ in cnodes_)
        if not is_auto_:
            continue
        has_bump_ = any(
            nodes[x_].get('kind') == 'redrive' and nodes[x_].get('bump')
            for x_ in cnodes_) or any(
            nodes[x_].get('kind') == 'route'
            and any(a_[0] == 'bump'
                    for b_ in nodes[x_].get('branches', [])
                    for a_ in actions(b_.get('do'), {'counters': {}, 'events': []}))
            for x_ in cnodes_)
        if not has_bump_:
            raise AssertionError(
                f'automatic cycle {sorted(cnodes_)} has no counter bump '
                'anywhere on it — unbounded self-loop (§2.2)')
    # per-edge rules
    for (u19, w19, bi27_) in list(guard_ctrs.keys()):
        if scc_of19.get(u19) != scc_of19.get(w19) or u19 not in scc_of19:
            continue
        gc_ = guard_ctrs[(u19, w19, bi27_)]
        src_n_ = nodes[u19]
        dst_n_ = nodes[w19]
        dst_br = [src_n_['branches'][bi27_]]
        node_b = dst_n_.get('bump') if dst_n_['kind'] == 'redrive' else None
        if node_b == gc_:
            for b_ in dst_br:
                for a_ in actions(b_.get('do'), {'counters': {}, 'events': []}):
                    if a_[0] == 'bump' and a_[1] == gc_:
                        raise AssertionError(f'edge {u19}->{w19}: counter {gc_!r} bumped BOTH on branch and node — double increment (§2.3)')
        else:
            for b_ in dst_br:
                has_ = any(a_[0] == 'bump' and a_[1] == gc_
                           for a_ in actions(b_.get('do'), {'counters': {}, 'events': []}))
                assert has_, (f'edge {u19}->{w19}: guarded cycle edge counter {gc_!r} '
                              'not bumped on every branch (§2.2/§2.3)')
    # (F23) every ROUTE branch re-entering an automatic SCC carries
    # its OWN guard — per-branch, not per-edge
    for u19, ws19 in auto_adj.items():
        if nodes.get(u19, {}).get('kind') != 'route':
            continue
        for w19 in ws19:
            if scc_of19.get(u19) == scc_of19.get(w19) and u19 in scc_of19:
                brs_ = [b_ for b_ in nodes[u19]['branches']
                        if b_.get('goto') == w19]
                for b_ in brs_:
                    wb_ = b_.get('when', '')
                    if not _re19.search(r'count_lt\s*\(', wb_):
                        raise AssertionError(
                            f'route {u19} branch to {w19} re-enters an '
                            'automatic cycle without a count_lt guard on '
                            'THAT branch (§2.2)')
                    # (F24-1) guard must hold POSITIVELY — count_lt under
                    # any negation is an exit condition, not a cap
                    depth_ = 0
                    neg_ = False
                    ok_ = True
                    instr_ = False
                    k_ = 0
                    while k_ < len(wb_):
                        ch_ = wb_[k_]
                        if ch_ == '"':
                            instr_ = not instr_
                        elif not instr_ and ch_ == '(':
                            depth_ += 1
                        elif not instr_ and ch_ == ')':
                            depth_ -= 1
                        elif (not instr_ and ch_ == '!'
                              and not neg_
                              and _re19.match(r'!\s*count_lt', wb_[k_:])):
                            neg_ = True
                        elif (not instr_ and ch_ == '!'
                              and wb_[k_ + 1:].lstrip().startswith('(')):
                            d2_ = 0
                            m2_ = k_ + 1
                            while m2_ < len(wb_):
                                if wb_[m2_] == '(':
                                    d2_ += 1
                                elif wb_[m2_] == ')':
                                    d2_ -= 1
                                    if d2_ == 0:
                                        break
                                m2_ += 1
                            if 'count_lt' in wb_[k_:m2_]:
                                ok_ = False
                        k_ += 1
                    if neg_ or not ok_:
                        raise AssertionError(
                            f'route {u19} branch to {w19}: count_lt under '
                            'negation — exit condition, not a re-entry cap (§2.2)')
                    # (F25-1) EVERY guard counter bumped on the cycle
                    # path from w19 BACK to u19
                    # (F28-2) per-BRANCH bump credit
                    for _k27 in guard_all_ctrs:
                        if not (_k27[0] == u19 and _k27[1] == w19):
                            continue
                        _br27 = nodes[u19]['branches'][_k27[2]]
                        for gc_all in guard_all_ctrs[_k27]:
                            if gc_all not in _re19.findall(
                                    r'count_lt\s*\(\s*([a-z_][a-z0-9_]*)',
                                    _br27.get('when', '')):
                                continue
                            b_here_ = any(
                                a_[0] == 'bump' and a_[1] == gc_all
                                for a_ in actions(_br27.get('do'), {'counters': {}, 'events': []}))
                            b_node_ = (nodes[w19].get('kind') == 'redrive'
                                       and nodes[w19].get('bump') == gc_all)
                            reach_ = set()
                            stack_ = [w19]
                            while stack_:
                                x_ = stack_.pop()
                                if x_ in reach_:
                                    continue
                                reach_.add(x_)
                                if x_ == u19:
                                    continue
                                for y_ in auto_adj.get(x_, ()):
                                    if y_ not in reach_:
                                        stack_.append(y_)
                            back_ = reach_ - {u19}
                            b_comp_ = any(gc_all in bump_ctr.get(x_, set())
                                          for x_ in back_)
                            if not (b_here_ or b_node_ or b_comp_):
                                raise AssertionError(
                                    f'route {u19} branch to {w19}: guard counter '
                                    f'{gc_all!r} never bumped along the cycle — '
                                    'OR-joined stale counter bypasses the cap (§2.2/§2.3)')
        # (F24-2) exactly-once: at most ONE redrive node per guard
        # counter inside an automatic SCC
        for u19, ws19 in auto_adj.items():
            if nodes.get(u19, {}).get('kind') != 'route':
                continue
            for w19 in ws19:
                if scc_of19.get(u19) == scc_of19.get(w19) and u19 in scc_of19:
                    _g27 = {v_ for k_, v_ in guard_ctrs.items()
                            if k_[0] == u19 and k_[1] == w19}
                    for _k27, _v27 in guard_all_ctrs.items():
                        if _k27[0] == u19 and _k27[1] == w19:
                            _g27 |= _v27
                    comp_ = set(sccs19[scc_of19[u19]])
                    for gc2_ in _g27:
                        red_ = [x_ for x_ in comp_
                                if nodes[x_].get('kind') == 'redrive'
                                and nodes[x_].get('bump') == gc2_]
                        rou_ = [x_ for x_ in comp_
                                if nodes[x_].get('kind') == 'route'
                                and gc2_ in bump_ctr.get(x_, set())]
                        owners_ = red_ + rou_
                        # (F27-2a) guard branch do bump + return path
                        br_do_bumps_ = any(
                            a_[0] == 'bump' and a_[1] == gc2_
                            for k_ in guard_ctrs
                            if k_[0] == u19 and k_[1] == w19
                            for br_ in [nodes[u19]['branches'][k_[2]]]
                            for a_ in actions(br_.get('do'), {'counters': {}, 'events': []}))
                        wpath_ = _reach_set19(auto_adj, w19, comp_ - {u19})
                        if br_do_bumps_ and any(
                                x_ != w19 and x_ in owners_ for x_ in wpath_):
                            raise AssertionError(
                                f'automatic cycle: counter {gc2_!r} bumped on '
                                'the guard branch AND on the return path — '
                                'double increment (§2.3)')
                        for a_ in range(len(owners_)):
                            for b_ in range(a_ + 1, len(owners_)):
                                oa_, ob_ = owners_[a_], owners_[b_]
                                if oa_ == ob_:
                                    continue
                                ab_ = ob_ in _reach_set19(auto_adj, oa_, comp_ - {u19})
                                ba_ = oa_ in _reach_set19(auto_adj, ob_, comp_ - {u19})
                                # (F28-1) serial one-directional reach
                                # also runs both bumps in one traversal
                                if ab_ or ba_:
                                    raise AssertionError(
                                        f'automatic cycle: counter {gc2_!r} '
                                        f'bumped by co-reachable nodes '
                                        f'{oa_},{ob_} — double increment (§2.3)')
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


    # (F16-2) cross-node revert->emit: a revert action's goto chain
    # must not reach an emit without passing a hold (re-verification)
    def reaches_emit(start, seen=None):
        if seen is None:
            seen = set()
        if start in seen or start not in nodes:
            return False
        seen.add(start)
        nd = nodes[start]
        if nd['kind'] == 'emit':
            return True
        if nd['kind'] == 'route':
            for br_ in nd['branches']:
                if br_.get('goto') and reaches_emit(br_['goto'], seen):
                    return True
                for a_ in actions(br_.get('do'), {'counters': {}, 'events': []}):
                    if a_[0] == 'emit' and not br_.get('goto'):
                        return True
        # hold AND dispatch re-execution both imply re-verification
        # (§2.3) — a revert path through either is legal
        return False  # hold = re-verification, legal
    for n_ in nodes.values():
        if n_['kind'] == 'route':
            for br_ in n_['branches']:
                acts_r = actions(br_.get('do'), {'counters': {}, 'events': []})
                if any(a_[0] == 'revert' for a_ in acts_r):
                    assert not reaches_emit(br_.get('goto')), \
                        f'revert path reaches emit (route {n_["id"]}, §2.3)'
    # (F3-3) emit payload_from validated after schemas load (main)
    # (F3-1/F4-1) top-level [hold] table is MANDATORY
    assert 'hold' in policy, 'pack must declare a top-level [hold] table'
    ht = policy['hold'].get('timeout_s')
    assert ht is not None, 'top-level [hold] must declare timeout_s (0=infinite)'
    assert isinstance(ht, int) and not isinstance(ht, bool) and ht >= 0, \
        'top-level [hold] timeout_s must be >= 0 (0=infinite)'
    import re as _re18
    for cname in policy.get('counters', {}):
        assert _re18.fullmatch(r'[a-z_][a-z0-9_]*', cname), \
            f'counter name {cname!r} not a valid identifier'
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
            for f in ('on_no_valid', 'on_verify_invalid', 'hold_target', 'on_done'):
                assert f in n, f'dispatch {n["id"]} missing {f}'
            assert n['hold_target'] in nodes, \
                f'dispatch {n["id"]} hold_target {n["hold_target"]!r} dangling'
            ht_ = nodes[n['hold_target']]
            assert ht_['kind'] == 'hold', \
                f'dispatch {n["id"]} hold_target must reference a hold node'
        if kind == 'hold':
            assert n.get('resume_targets') is not None, \
                f'hold {n["id"]} missing resume_targets'
        refs = [n[k] for k in ('on_done', 'hold_target') if k in n]
        if kind == 'route':
            assert 'on_done' not in n, f'route {n["id"]} must not declare on_done (§2.3)'
            assert n['branches'][-1]['when'].strip() == 'true', 'missing last true fallback'
            for b in n['branches']:
                # (F15-4) declared counters seeded so count_lt on a
                # pack-declared counter cannot KeyError
                _st = state()
                # (F24-4) syntax-check state contains ONLY pack-declared
                # counters — the state() convenience seeds must not
                # mask undeclared-counter usage after a rename
                _st['counters'] = {c: 0 for c in policy.get('counters', {})}
                try:
                    Expr(b['when'], _st).run()
                except KeyError as _ke:
                    raise AssertionError(
                        f'branch references undeclared counter {_ke.args[0]!r}')
                acts_ = actions(b.get('do'), state())
                # (F11-3) bump targets must be DECLARED — dead branches too
                for a_ in acts_:
                    if a_[0] == 'bump':
                        assert a_[1] in policy.get('counters', {}), \
                            f'branch bumps undeclared counter {a_[1]!r} (route {n["id"]})'
                    if a_[0] == 'emit':
                        assert a_[1] in ('InspectionPassed', 'InspectionFailed'), \
                            f'emit action event {a_[1]!r} not canonical (route {n["id"]})'
                    if a_[0] == 'revert':
                        assert not any(x[0] == 'emit' for x in acts_[acts_.index(a_)+1:]), \
                            f'revert-then-emit (route {n["id"]}, §2.3)'
                if acts_ and not b.get('goto'):
                    assert acts_[-1][0] == 'emit', \
                        f'route {n["id"]} branch has actions but no goto (non-emit dead-end)'
                if not any(a_[0] == 'emit' for a_ in acts_):
                    g_ = b.get('goto')
                    assert g_, f'route {n["id"]} non-emit branch missing goto'
                    assert g_ in nodes, f'route {n["id"]} goto {g_!r} dangling'
                # (F10-1) emit is terminal: nothing after, no goto
                emits_ = [i for i, a in enumerate(acts_) if a[0] == 'emit']
                for i_ in emits_:
                    assert i_ == len(acts_) - 1, \
                        f'route {n["id"]} branch has action after emit'
                    assert not b.get('goto'), \
                        f'route {n["id"]} branch emits AND gotos (§2.3)'
                assert len(emits_) <= 1, f'route {n["id"]} branch multi-emit (§2.3)'
                # (F14-3) an emitting branch has no goto — only append
                # refs for branches that actually carry one
                if b.get('goto'):
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
    global PACK_DIR_
    PACK_DIR_ = args.pack
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
