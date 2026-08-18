#!/usr/bin/env python3
"""Generate the maintained Caldera simulation-engine stress program.

Caldera deliberately contains large families of audit passes, systems,
behaviors, content rules, and report adapters.  The generator keeps those
families deterministic and reviewable while each generated module still has a
distinct identity, formula, and executable registration in its canonical
suite.  Run this script from anywhere in the Rue repository.
"""

from __future__ import annotations

import argparse
import hashlib
from pathlib import Path
from textwrap import dedent


REPO = Path(__file__).resolve().parents[1]
OUT = REPO / "examples" / "caldera"

def clean(text: str) -> str:
    return dedent(text).strip() + "\n"


def write(name: str, text: str) -> None:
    path = OUT / name
    path.parent.mkdir(parents=True, exist_ok=True)
    path.write_text(clean(text), encoding="utf-8")


CORE: dict[str, str] = {
    "metric.rue": r'''
        // Deterministic measurements shared by every Caldera subsystem.
        const std = @import("std");
        const StrBuf = std.strbuf.StrBuf;

        pub struct Metric {
            tag: u64,
            observations: u64,
            total: u64,
            minimum: u64,
            maximum: u64,
            warnings: u64,
            digest: u64,
            valid: bool,
        }

        pub fn mix(value: u64, next: u64) -> u64 {
            @wrapping_mul(value ^ next, 1099511628211)
        }

        pub fn new(tag: u64) -> Metric {
            Metric { tag: tag, observations: 0, total: 0,
                minimum: 18446744073709551615, maximum: 0, warnings: 0,
                digest: mix(14695981039346656037, tag), valid: false }
        }

        pub fn observe(inout value: Metric, sample: u64, weight: u64) {
            value.observations += 1;
            value.total = @wrapping_add(value.total, @wrapping_mul(sample, weight));
            if sample < value.minimum { value.minimum = sample; }
            if sample > value.maximum { value.maximum = sample; }
            value.digest = mix(value.digest, sample);
            value.digest = mix(value.digest, weight);
        }

        pub fn warn(inout value: Metric, condition: bool) {
            if condition {
                value.warnings += 1;
                value.digest = mix(value.digest, 18446744073709551557);
            }
        }

        pub fn finish(inout value: Metric) {
            if value.observations == 0 { value.minimum = 0; }
            value.digest = mix(value.digest, value.observations);
            value.digest = mix(value.digest, value.total);
            value.digest = mix(value.digest, value.minimum);
            value.digest = mix(value.digest, value.maximum);
            value.digest = mix(value.digest, value.warnings);
            value.valid = value.observations > 0 && value.warnings == 0;
        }

        pub fn valid(borrow value: Metric) -> bool {
            value.valid && value.observations > 0 && value.minimum <= value.maximum
        }

        pub fn average(borrow value: Metric) -> u64 {
            if value.observations == 0 { 0 } else { value.total / value.observations }
        }

        pub fn combine(borrow left: Metric, borrow right: Metric) -> Metric {
            let mut out = new(mix(left.tag, right.tag));
            observe(inout out, left.total, left.observations + 1);
            observe(inout out, right.total, right.observations + 1);
            out.warnings = left.warnings + right.warnings;
            out.digest = mix(out.digest, left.digest);
            out.digest = mix(out.digest, right.digest);
            finish(inout out);
            out.valid = left.valid && right.valid && out.warnings == 0;
            out
        }
    ''',
    "text.rue": r'''
        // Text building and stable hashing without a formatting runtime.
        const std = @import("std");
        const StrBuf = std.strbuf.StrBuf;

        pub fn append_literal(inout out: StrBuf, value: str) {
            let mut i: u64 = 0;
            while i < value.len() { out.push(value[i]); i += 1; }
        }

        pub fn append_string(inout out: StrBuf, borrow value: StrBuf) {
            let mut i: u64 = 0;
            while i < value.len() {
                out.push(StrBuf.byte_at_borrowed(borrow value, i));
                i += 1;
            }
        }

        pub fn append_u64(inout out: StrBuf, value: u64) {
            let rendered = @to_string(value);
            append_string(inout out, borrow rendered);
        }

        pub fn append_i64(inout out: StrBuf, value: i64) {
            let rendered = @to_string(value);
            append_string(inout out, borrow rendered);
        }

        pub fn append_bool(inout out: StrBuf, value: bool) {
            if value { append_literal(inout out, "true"); }
            else { append_literal(inout out, "false"); }
        }

        pub fn line(inout out: StrBuf, label: str, value: u64) {
            append_literal(inout out, label); append_u64(inout out, value); out.push(10);
        }

        pub fn signed_line(inout out: StrBuf, label: str, value: i64) {
            append_literal(inout out, label); append_i64(inout out, value); out.push(10);
        }

        pub fn bool_line(inout out: StrBuf, label: str, value: bool) {
            append_literal(inout out, label); append_bool(inout out, value); out.push(10);
        }

        pub fn fnv1a(borrow value: StrBuf) -> u64 {
            let mut hash: u64 = 14695981039346656037;
            let mut i: u64 = 0;
            while i < value.len() {
                hash ^= @intCast(StrBuf.byte_at_borrowed(borrow value, i));
                hash = @wrapping_mul(hash, 1099511628211);
                i += 1;
            }
            hash
        }
    ''',
    "fixed.rue": r'''
        // Signed fixed-point arithmetic with three decimal places.
        pub fn SCALE() -> i64 { 1000 }

        pub struct Fixed { raw: i64 }

        pub fn zero() -> Fixed { Fixed { raw: 0 } }
        pub fn from_int(value: i64) -> Fixed { Fixed { raw: value * SCALE() } }
        pub fn from_parts(integer: i64, milli: i64) -> Fixed {
            Fixed { raw: integer * SCALE() + milli }
        }
        pub fn add(borrow a: Fixed, borrow b: Fixed) -> Fixed { Fixed { raw: a.raw + b.raw } }
        pub fn sub(borrow a: Fixed, borrow b: Fixed) -> Fixed { Fixed { raw: a.raw - b.raw } }
        pub fn mul(borrow a: Fixed, borrow b: Fixed) -> Fixed {
            Fixed { raw: (a.raw * b.raw) / SCALE() }
        }
        pub fn div(borrow a: Fixed, borrow b: Fixed) -> Fixed {
            if b.raw == 0 { zero() } else { Fixed { raw: (a.raw * SCALE()) / b.raw } }
        }
        pub fn abs(borrow value: Fixed) -> Fixed {
            Fixed { raw: if value.raw < 0 { 0 - value.raw } else { value.raw } }
        }
        pub fn clamp(borrow value: Fixed, borrow low: Fixed, borrow high: Fixed) -> Fixed {
            if value.raw < low.raw { Fixed { raw: low.raw } }
            else if value.raw > high.raw { Fixed { raw: high.raw } }
            else { Fixed { raw: value.raw } }
        }
        pub fn floor(borrow value: Fixed) -> i64 { value.raw / SCALE() }
    ''',
    "vector.rue": r'''
        // Fixed-point vectors used by movement, steering, and collision.
        const fixed = @import("fixed.rue");

        pub struct Vec2 { x: fixed.Fixed, y: fixed.Fixed }

        pub fn zero() -> Vec2 { Vec2 { x: fixed.zero(), y: fixed.zero() } }
        pub fn from_ints(x: i64, y: i64) -> Vec2 {
            Vec2 { x: fixed.from_int(x), y: fixed.from_int(y) }
        }
        pub fn add(borrow a: Vec2, borrow b: Vec2) -> Vec2 {
            Vec2 { x: fixed.add(borrow a.x, borrow b.x), y: fixed.add(borrow a.y, borrow b.y) }
        }
        pub fn sub(borrow a: Vec2, borrow b: Vec2) -> Vec2 {
            Vec2 { x: fixed.sub(borrow a.x, borrow b.x), y: fixed.sub(borrow a.y, borrow b.y) }
        }
        pub fn scale(borrow a: Vec2, borrow amount: fixed.Fixed) -> Vec2 {
            Vec2 { x: fixed.mul(borrow a.x, borrow amount), y: fixed.mul(borrow a.y, borrow amount) }
        }
        pub fn manhattan(borrow a: Vec2, borrow b: Vec2) -> u64 {
            let dx = fixed.abs(borrow fixed.sub(borrow a.x, borrow b.x));
            let dy = fixed.abs(borrow fixed.sub(borrow a.y, borrow b.y));
            @intCast((dx.raw + dy.raw) / fixed.SCALE())
        }
        pub fn equal(borrow a: Vec2, borrow b: Vec2) -> bool {
            a.x.raw == b.x.raw && a.y.raw == b.y.raw
        }
    ''',
    "model.rue": r'''
        // Canonical compact-index simulation state. All generated passes consume
        // this representation, keeping one source of truth across Caldera.
        const std = @import("std");

        pub fn NONE() -> u64 { 18446744073709551615 }
        pub fn WORKER() -> u64 { 1 }
        pub fn SCOUT() -> u64 { 2 }
        pub fn BUILDER() -> u64 { 3 }
        pub fn GUARD() -> u64 { 4 }
        pub fn PLAIN() -> u64 { 1 }
        pub fn WALL() -> u64 { 2 }
        pub fn WATER() -> u64 { 3 }
        pub fn FOREST() -> u64 { 4 }

        pub struct Entity {
            id: u64, kind: u64, faction: u64,
            x: i64, y: i64, previous_x: i64, previous_y: i64,
            velocity_x: i64, velocity_y: i64,
            health: u64, energy: u64, state: u64, goal: u64,
            inventory: u64, credits: u64, experience: u64,
            born_tick: u64, updated_tick: u64, alive: bool,
        }

        pub struct Tile {
            x: u64, y: u64, terrain: u64, height: u64,
            resource: u64, occupant: u64, region: u64,
            walkable: bool, visible: bool,
        }

        pub struct Event {
            tick: u64, kind: u64, actor: u64, target: u64,
            value: u64, sequence: u64,
        }

        pub struct Command {
            tick: u64, kind: u64, actor: u64,
            x: i64, y: i64, value: u64, accepted: bool,
        }

        pub struct Snapshot {
            tick: u64, entity_count: u64, event_count: u64,
            command_count: u64, checksum: u64,
        }

        pub struct Diagnostic { code: u64, subject: u64, detail: u64 }

        pub const Entities = std.arraybuf.ArrayBuf(Entity);
        pub const Tiles = std.arraybuf.ArrayBuf(Tile);
        pub const Events = std.arraybuf.ArrayBuf(Event);
        pub const Commands = std.arraybuf.ArrayBuf(Command);
        pub const Snapshots = std.arraybuf.ArrayBuf(Snapshot);
        pub const Diagnostics = std.arraybuf.ArrayBuf(Diagnostic);
        pub const U64s = std.arraybuf.ArrayBuf(u64);
        pub const I64s = std.arraybuf.ArrayBuf(i64);
        pub const Bools = std.arraybuf.ArrayBuf(bool);

        pub struct World {
            seed: u64, tick: u64, width: u64, height: u64,
            entities: Entities, tiles: Tiles, events: Events,
            commands: Commands, snapshots: Snapshots,
            diagnostics: Diagnostics,
            resources: u64, treasury: u64, quests_completed: u64,
            script_steps: u64, rollback_count: u64,
            next_entity: u64, next_sequence: u64, valid: bool,
        }

        pub fn new_world(seed: u64, width: u64, height: u64) -> World {
            World { seed: seed, tick: 0, width: width, height: height,
                entities: Entities.new(), tiles: Tiles.new(), events: Events.new(),
                commands: Commands.new(), snapshots: Snapshots.new(),
                diagnostics: Diagnostics.new(), resources: 0, treasury: 0,
                quests_completed: 0, script_steps: 0, rollback_count: 0,
                next_entity: 1, next_sequence: 1, valid: true }
        }

        pub fn empty_entity() -> Entity {
            Entity { id: NONE(), kind: 0, faction: 0, x: 0, y: 0,
                previous_x: 0, previous_y: 0, velocity_x: 0, velocity_y: 0,
                health: 0, energy: 0, state: 0, goal: NONE(), inventory: 0,
                credits: 0, experience: 0, born_tick: 0, updated_tick: 0,
                alive: false }
        }

        pub fn empty_tile() -> Tile {
            Tile { x: 0, y: 0, terrain: PLAIN(), height: 0, resource: 0,
                occupant: NONE(), region: 0, walkable: true, visible: false }
        }

        pub fn empty_event() -> Event {
            Event { tick: 0, kind: 0, actor: NONE(), target: NONE(),
                value: 0, sequence: 0 }
        }

        pub fn empty_command() -> Command {
            Command { tick: 0, kind: 0, actor: NONE(), x: 0, y: 0,
                value: 0, accepted: false }
        }

        pub fn empty_snapshot() -> Snapshot {
            Snapshot { tick: 0, entity_count: 0, event_count: 0,
                command_count: 0, checksum: 0 }
        }

        pub fn tile_index(borrow world: World, x: u64, y: u64) -> u64 {
            if x >= world.width || y >= world.height { NONE() }
            else { y * world.width + x }
        }

        pub fn find_entity(borrow world: World, id: u64) -> u64 {
            let mut i: u64 = 0;
            while i < world.entities.len() {
                if world.entities.get_or(i, empty_entity()).id == id { return i; }
                i += 1;
            }
            NONE()
        }

        pub fn alive_count(borrow world: World) -> u64 {
            let mut count: u64 = 0;
            let mut i: u64 = 0;
            while i < world.entities.len() {
                if world.entities.get_or(i, empty_entity()).alive { count += 1; }
                i += 1;
            }
            count
        }

        pub fn walkable(borrow world: World, x: i64, y: i64) -> bool {
            if x < 0 || y < 0 { return false; }
            let ux: u64 = @intCast(x);
            let uy: u64 = @intCast(y);
            let index = tile_index(borrow world, ux, uy);
            index != NONE() && world.tiles.get_or(index, empty_tile()).walkable
        }

        pub fn add_event(inout world: World, kind: u64, actor: u64,
            target: u64, value: u64) {
            world.events.push(Event { tick: world.tick, kind: kind, actor: actor,
                target: target, value: value, sequence: world.next_sequence });
            world.next_sequence += 1;
        }

        pub fn add_diagnostic(inout world: World, code: u64, subject: u64, detail: u64) {
            world.diagnostics.push(Diagnostic { code: code, subject: subject, detail: detail });
            world.valid = false;
        }
    ''',
    "scenario_model.rue": r'''
        // Parsed scenario declarations stay separate from live world state.
        const std = @import("std");

        pub struct EntitySpec { kind: u64, faction: u64, x: u64, y: u64 }
        pub struct TileSpec { x: u64, y: u64, terrain: u64 }
        pub const EntitySpecs = std.arraybuf.ArrayBuf(EntitySpec);
        pub const TileSpecs = std.arraybuf.ArrayBuf(TileSpec);

        pub struct Scenario {
            width: u64, height: u64, ticks: u64,
            entities: EntitySpecs, tiles: TileSpecs,
            expected_entities: u64, errors: u64, valid: bool,
        }

        pub fn new() -> Scenario {
            Scenario { width: 8, height: 8, ticks: 4,
                entities: EntitySpecs.new(), tiles: TileSpecs.new(),
                expected_entities: 0, errors: 0, valid: true }
        }

        pub fn empty_entity() -> EntitySpec { EntitySpec { kind: 0, faction: 0, x: 0, y: 0 } }
        pub fn empty_tile() -> TileSpec { TileSpec { x: 0, y: 0, terrain: 0 } }
    ''',
    "token.rue": r'''
        const std = @import("std");
        pub fn EOF() -> u64 { 0 }
        pub fn NUMBER() -> u64 { 1 }
        pub fn WORLD() -> u64 { 2 }
        pub fn ENTITY() -> u64 { 3 }
        pub fn TILE() -> u64 { 4 }
        pub fn TICK() -> u64 { 5 }
        pub fn EXPECT() -> u64 { 6 }
        pub fn WORKER() -> u64 { 7 }
        pub fn SCOUT() -> u64 { 8 }
        pub fn BUILDER() -> u64 { 9 }
        pub fn GUARD() -> u64 { 10 }
        pub fn PLAIN() -> u64 { 11 }
        pub fn WALL() -> u64 { 12 }
        pub fn WATER() -> u64 { 13 }
        pub fn FOREST() -> u64 { 14 }
        pub fn ENTITIES() -> u64 { 15 }
        pub fn SEMI() -> u64 { 16 }
        pub fn INVALID() -> u64 { 17 }

        pub struct Token { kind: u64, number: u64, offset: u64 }
        pub const Tokens = std.arraybuf.ArrayBuf(Token);
        pub fn empty() -> Token { Token { kind: INVALID(), number: 0, offset: 0 } }
    ''',
    "lexer.rue": r'''
        // Lexer for Caldera's compact deterministic scenario language.
        const std = @import("std");
        const token = @import("token.rue");
        const Bytes = std.arraybuf.ArrayBuf(u8);

        pub struct Scan { tokens: token.Tokens, errors: u64 }

        fn alpha(byte: u8) -> bool {
            (byte >= 65 && byte <= 90) || (byte >= 97 && byte <= 122) || byte == 95
        }
        fn digit(byte: u8) -> bool { byte >= 48 && byte <= 57 }
        fn upper(byte: u8) -> u8 { if byte >= 97 && byte <= 122 { byte - 32 } else { byte } }

        fn word_eq(borrow source: Bytes, start: u64, end: u64, value: str) -> bool {
            if end - start != value.len() { return false; }
            let mut i: u64 = 0;
            while i < value.len() {
                if upper(source.get_or(start + i, 0)) != value[i] { return false; }
                i += 1;
            }
            true
        }

        fn word_kind(borrow source: Bytes, start: u64, end: u64) -> u64 {
            if word_eq(borrow source, start, end, "WORLD") { token.WORLD() }
            else if word_eq(borrow source, start, end, "ENTITY") { token.ENTITY() }
            else if word_eq(borrow source, start, end, "TILE") { token.TILE() }
            else if word_eq(borrow source, start, end, "TICK") { token.TICK() }
            else if word_eq(borrow source, start, end, "EXPECT") { token.EXPECT() }
            else if word_eq(borrow source, start, end, "WORKER") { token.WORKER() }
            else if word_eq(borrow source, start, end, "SCOUT") { token.SCOUT() }
            else if word_eq(borrow source, start, end, "BUILDER") { token.BUILDER() }
            else if word_eq(borrow source, start, end, "GUARD") { token.GUARD() }
            else if word_eq(borrow source, start, end, "PLAIN") { token.PLAIN() }
            else if word_eq(borrow source, start, end, "WALL") { token.WALL() }
            else if word_eq(borrow source, start, end, "WATER") { token.WATER() }
            else if word_eq(borrow source, start, end, "FOREST") { token.FOREST() }
            else if word_eq(borrow source, start, end, "ENTITIES") { token.ENTITIES() }
            else { token.INVALID() }
        }

        pub fn scan(source: Bytes) -> Scan {
            let mut out = Scan { tokens: token.Tokens.new(), errors: 0 };
            let mut i: u64 = 0;
            while i < source.len() {
                let byte = source.get_or(i, 0);
                if byte == 32 || byte == 9 || byte == 10 || byte == 13 {
                    i += 1;
                } else if byte == 35 {
                    while i < source.len() && source.get_or(i, 0) != 10 { i += 1; }
                } else if byte == 59 {
                    out.tokens.push(token.Token { kind: token.SEMI(), number: 0, offset: i });
                    i += 1;
                } else if digit(byte) {
                    let start = i;
                    let mut number: u64 = 0;
                    while i < source.len() && digit(source.get_or(i, 0)) {
                        number = number * 10 + @intCast(source.get_or(i, 0) - 48);
                        i += 1;
                    }
                    out.tokens.push(token.Token { kind: token.NUMBER(), number: number, offset: start });
                } else if alpha(byte) {
                    let start = i;
                    while i < source.len() && alpha(source.get_or(i, 0)) { i += 1; }
                    let kind = word_kind(borrow source, start, i);
                    if kind == token.INVALID() { out.errors += 1; }
                    out.tokens.push(token.Token { kind: kind, number: 0, offset: start });
                } else {
                    out.errors += 1;
                    out.tokens.push(token.Token { kind: token.INVALID(), number: 0, offset: i });
                    i += 1;
                }
            }
            out.tokens.push(token.Token { kind: token.EOF(), number: 0, offset: source.len() });
            out
        }
    ''',
    "parser.rue": r'''
        // Predictive parser and validator for scenario declarations.
        const model = @import("model.rue");
        const scenario = @import("scenario_model.rue");
        const token = @import("token.rue");

        struct State { tokens: token.Tokens, cursor: u64, errors: u64 }

        fn peek(borrow state: State) -> token.Token {
            state.tokens.get_or(state.cursor, token.empty())
        }
        fn advance(inout state: State) -> token.Token {
            let value = peek(borrow state);
            if state.cursor < state.tokens.len() { state.cursor += 1; }
            value
        }
        fn take(inout state: State, kind: u64) -> token.Token {
            let value = advance(inout state);
            if value.kind != kind { state.errors += 1; }
            value
        }
        fn entity_kind(kind: u64) -> u64 {
            if kind == token.WORKER() { model.WORKER() }
            else if kind == token.SCOUT() { model.SCOUT() }
            else if kind == token.BUILDER() { model.BUILDER() }
            else if kind == token.GUARD() { model.GUARD() }
            else { 0 }
        }
        fn terrain_kind(kind: u64) -> u64 {
            if kind == token.PLAIN() { model.PLAIN() }
            else if kind == token.WALL() { model.WALL() }
            else if kind == token.WATER() { model.WATER() }
            else if kind == token.FOREST() { model.FOREST() }
            else { 0 }
        }

        pub fn parse(tokens: token.Tokens) -> scenario.Scenario {
            let mut state = State { tokens: tokens, cursor: 0, errors: 0 };
            let mut out = scenario.new();
            while peek(borrow state).kind != token.EOF() {
                let statement = advance(inout state);
                if statement.kind == token.WORLD() {
                    out.width = take(inout state, token.NUMBER()).number;
                    out.height = take(inout state, token.NUMBER()).number;
                    let _ = take(inout state, token.SEMI());
                } else if statement.kind == token.ENTITY() {
                    let kind_token = advance(inout state);
                    let faction = take(inout state, token.NUMBER()).number;
                    let x = take(inout state, token.NUMBER()).number;
                    let y = take(inout state, token.NUMBER()).number;
                    let _ = take(inout state, token.SEMI());
                    let kind = entity_kind(kind_token.kind);
                    if kind == 0 { state.errors += 1; }
                    out.entities.push(scenario.EntitySpec { kind: kind, faction: faction, x: x, y: y });
                } else if statement.kind == token.TILE() {
                    let x = take(inout state, token.NUMBER()).number;
                    let y = take(inout state, token.NUMBER()).number;
                    let terrain_token = advance(inout state);
                    let _ = take(inout state, token.SEMI());
                    let terrain = terrain_kind(terrain_token.kind);
                    if terrain == 0 { state.errors += 1; }
                    out.tiles.push(scenario.TileSpec { x: x, y: y, terrain: terrain });
                } else if statement.kind == token.TICK() {
                    out.ticks = take(inout state, token.NUMBER()).number;
                    let _ = take(inout state, token.SEMI());
                } else if statement.kind == token.EXPECT() {
                    let _ = take(inout state, token.ENTITIES());
                    out.expected_entities = take(inout state, token.NUMBER()).number;
                    let _ = take(inout state, token.SEMI());
                } else {
                    state.errors += 1;
                    while peek(borrow state).kind != token.SEMI() &&
                        peek(borrow state).kind != token.EOF() { let _ = advance(inout state); }
                    if peek(borrow state).kind == token.SEMI() { let _ = advance(inout state); }
                }
            }
            out.errors = state.errors;
            out.valid = state.errors == 0 && out.width > 0 && out.height > 0 &&
                out.entities.len() > 0 && (out.expected_entities == 0 ||
                    out.expected_entities == out.entities.len());
            out
        }
    ''',
    "worldgen.rue": r'''
        // Deterministic terrain, entity, and scenario materialization.
        const model = @import("model.rue");
        const scenario = @import("scenario_model.rue");

        fn terrain(seed: u64, x: u64, y: u64) -> u64 {
            let value = @wrapping_mul(seed ^ (x + 17) ^ @wrapping_mul(y + 31, 97), 1099511628211);
            if value % 23 == 0 { model.WATER() }
            else if value % 17 == 0 { model.FOREST() }
            else { model.PLAIN() }
        }

        fn add_tiles(inout world: model.World) {
            let mut y: u64 = 0;
            while y < world.height {
                let mut x: u64 = 0;
                while x < world.width {
                    let kind = terrain(world.seed, x, y);
                    world.tiles.push(model.Tile { x: x, y: y, terrain: kind,
                        height: (x * 7 + y * 11 + world.seed) % 19,
                        resource: (x * 13 + y * 5 + world.seed) % 29,
                        occupant: model.NONE(), region: (x / 4) + (y / 4) * 8,
                        walkable: kind != model.WATER() && kind != model.WALL(),
                        visible: false });
                    x += 1;
                }
                y += 1;
            }
        }

        pub fn add_entity(inout world: model.World, kind: u64, faction: u64,
            x: u64, y: u64) -> u64 {
            let id = world.next_entity;
            world.next_entity += 1;
            world.entities.push(model.Entity { id: id, kind: kind, faction: faction,
                x: @intCast(x), y: @intCast(y), previous_x: @intCast(x), previous_y: @intCast(y),
                velocity_x: 0, velocity_y: 0, health: 100, energy: 100,
                state: 1, goal: model.NONE(), inventory: kind * 2,
                credits: 10 + faction * 3, experience: 0,
                born_tick: world.tick, updated_tick: world.tick, alive: true });
            let tile = model.tile_index(borrow world, x, y);
            if tile != model.NONE() {
                let current = world.tiles.get_or(tile, model.empty_tile());
                world.tiles.set(tile, model.Tile { x: current.x, y: current.y,
                    terrain: current.terrain, height: current.height,
                    resource: current.resource, occupant: id, region: current.region,
                    walkable: current.walkable, visible: true });
            }
            id
        }

        pub fn demo(scale: u64) -> model.World {
            let size = 8 + scale * 2;
            let mut world = model.new_world(0xCADA1078 + scale, size, size);
            add_tiles(inout world);
            let mut i: u64 = 0;
            while i < 8 * scale {
                let kind = (i % 4) + 1;
                let faction = (i % 3) + 1;
                let x = 1 + (i * 3) % (size - 2);
                let y = 1 + (i * 5) % (size - 2);
                let _ = add_entity(inout world, kind, faction, x, y);
                i += 1;
            }
            world.resources = 400 * scale;
            world.treasury = 250 * scale;
            world
        }

        pub fn from_scenario(borrow input: scenario.Scenario) -> model.World {
            let mut world = model.new_world(0xCADA1078, input.width, input.height);
            add_tiles(inout world);
            let mut i: u64 = 0;
            while i < input.tiles.len() {
                let spec = input.tiles.get_or(i, scenario.empty_tile());
                let index = model.tile_index(borrow world, spec.x, spec.y);
                if index == model.NONE() { model.add_diagnostic(inout world, 1001, i, 1); }
                else {
                    let old = world.tiles.get_or(index, model.empty_tile());
                    world.tiles.set(index, model.Tile { x: old.x, y: old.y,
                        terrain: spec.terrain, height: old.height, resource: old.resource,
                        occupant: old.occupant, region: old.region,
                        walkable: spec.terrain != model.WALL() && spec.terrain != model.WATER(),
                        visible: old.visible });
                }
                i += 1;
            }
            i = 0;
            while i < input.entities.len() {
                let spec = input.entities.get_or(i, scenario.empty_entity());
                if spec.x >= world.width || spec.y >= world.height {
                    model.add_diagnostic(inout world, 1002, i, 2);
                } else {
                    let _ = add_entity(inout world, spec.kind, spec.faction, spec.x, spec.y);
                }
                i += 1;
            }
            world.resources = input.entities.len() * 50;
            world.treasury = input.entities.len() * 25;
            world
        }
    ''',
}

CORE.update({
    "checksum.rue": r'''
        // Canonical whole-world checksum used by replay and rollback oracles.
        const metric = @import("metric.rue");
        const model = @import("model.rue");

        pub fn world(borrow value: model.World) -> u64 {
            let mut hash = metric.mix(14695981039346656037, value.seed);
            hash = metric.mix(hash, value.tick);
            hash = metric.mix(hash, value.width);
            hash = metric.mix(hash, value.height);
            hash = metric.mix(hash, value.resources);
            hash = metric.mix(hash, value.treasury);
            hash = metric.mix(hash, value.quests_completed);
            hash = metric.mix(hash, value.script_steps);
            let mut i: u64 = 0;
            while i < value.entities.len() {
                let entity = value.entities.get_or(i, model.empty_entity());
                hash = metric.mix(hash, entity.id);
                hash = metric.mix(hash, entity.kind);
                hash = metric.mix(hash, entity.faction);
                hash = metric.mix(hash, @intCast(entity.x + 4096));
                hash = metric.mix(hash, @intCast(entity.y + 4096));
                hash = metric.mix(hash, @intCast(entity.velocity_x + 4096));
                hash = metric.mix(hash, @intCast(entity.velocity_y + 4096));
                hash = metric.mix(hash, entity.health);
                hash = metric.mix(hash, entity.energy);
                hash = metric.mix(hash, entity.state);
                hash = metric.mix(hash, entity.inventory);
                hash = metric.mix(hash, entity.credits);
                hash = metric.mix(hash, entity.experience);
                hash = metric.mix(hash, if entity.alive { 1 } else { 0 });
                i += 1;
            }
            i = 0;
            while i < value.tiles.len() {
                let tile = value.tiles.get_or(i, model.empty_tile());
                hash = metric.mix(hash, tile.x);
                hash = metric.mix(hash, tile.y);
                hash = metric.mix(hash, tile.terrain);
                hash = metric.mix(hash, tile.height);
                hash = metric.mix(hash, tile.resource);
                hash = metric.mix(hash, tile.region);
                hash = metric.mix(hash, if tile.walkable { 1 } else { 0 });
                i += 1;
            }
            i = 0;
            while i < value.events.len() {
                let event = value.events.get_or(i, model.empty_event());
                hash = metric.mix(hash, event.tick);
                hash = metric.mix(hash, event.kind);
                hash = metric.mix(hash, event.actor);
                hash = metric.mix(hash, event.target);
                hash = metric.mix(hash, event.value);
                hash = metric.mix(hash, event.sequence);
                i += 1;
            }
            hash
        }
    ''',
    "geometry.rue": r'''
        const vector = @import("vector.rue");

        pub struct Box { minimum: vector.Vec2, maximum: vector.Vec2 }

        pub fn from_points(borrow a: vector.Vec2, borrow b: vector.Vec2) -> Box {
            let min_x = if a.x.raw < b.x.raw { a.x } else { b.x };
            let min_y = if a.y.raw < b.y.raw { a.y } else { b.y };
            let max_x = if a.x.raw > b.x.raw { a.x } else { b.x };
            let max_y = if a.y.raw > b.y.raw { a.y } else { b.y };
            Box { minimum: vector.Vec2 { x: min_x, y: min_y },
                maximum: vector.Vec2 { x: max_x, y: max_y } }
        }

        pub fn overlaps(borrow a: Box, borrow b: Box) -> bool {
            a.minimum.x.raw <= b.maximum.x.raw && a.maximum.x.raw >= b.minimum.x.raw &&
                a.minimum.y.raw <= b.maximum.y.raw && a.maximum.y.raw >= b.minimum.y.raw
        }

        pub fn contains(borrow area: Box, borrow point: vector.Vec2) -> bool {
            point.x.raw >= area.minimum.x.raw && point.x.raw <= area.maximum.x.raw &&
                point.y.raw >= area.minimum.y.raw && point.y.raw <= area.maximum.y.raw
        }
    ''',
    "spatial.rue": r'''
        // Grid-backed spatial queries with a brute-force verification path.
        const metric = @import("metric.rue");
        const model = @import("model.rue");

        pub struct Query { candidates: u64, matches: u64, digest: u64, valid: bool }

        pub fn region(borrow world: model.World, x: i64, y: i64, radius: u64) -> Query {
            let mut out = Query { candidates: 0, matches: 0,
                digest: metric.mix(14695981039346656037, radius), valid: false };
            let mut i: u64 = 0;
            while i < world.entities.len() {
                let entity = world.entities.get_or(i, model.empty_entity());
                out.candidates += 1;
                let dx = if entity.x > x { entity.x - x } else { x - entity.x };
                let dy = if entity.y > y { entity.y - y } else { y - entity.y };
                if entity.alive && @intCast(dx + dy) <= radius {
                    out.matches += 1;
                    out.digest = metric.mix(out.digest, entity.id);
                }
                i += 1;
            }
            out.valid = out.matches <= out.candidates;
            out
        }

        pub fn nearest(borrow world: model.World, x: i64, y: i64) -> u64 {
            let mut best = model.NONE();
            let mut distance = model.NONE();
            let mut i: u64 = 0;
            while i < world.entities.len() {
                let entity = world.entities.get_or(i, model.empty_entity());
                let dx = if entity.x > x { entity.x - x } else { x - entity.x };
                let dy = if entity.y > y { entity.y - y } else { y - entity.y };
                let current: u64 = @intCast(dx + dy);
                if entity.alive && (best == model.NONE() || current < distance ||
                    (current == distance && entity.id < best)) {
                    best = entity.id; distance = current;
                }
                i += 1;
            }
            best
        }
    ''',
    "collision.rue": r'''
        // Tile and entity collision checks are deterministic and order-stable.
        const metric = @import("metric.rue");
        const model = @import("model.rue");

        pub struct Audit { tile_hits: u64, entity_hits: u64, pairs: u64,
            digest: u64, valid: bool }

        pub fn audit(borrow world: model.World) -> Audit {
            let mut out = Audit { tile_hits: 0, entity_hits: 0, pairs: 0,
                digest: 14695981039346656037, valid: false };
            let mut i: u64 = 0;
            while i < world.entities.len() {
                let entity = world.entities.get_or(i, model.empty_entity());
                if entity.alive && !model.walkable(borrow world, entity.x, entity.y) {
                    out.tile_hits += 1;
                    out.digest = metric.mix(out.digest, entity.id);
                }
                let mut j = i + 1;
                while j < world.entities.len() {
                    let other = world.entities.get_or(j, model.empty_entity());
                    out.pairs += 1;
                    if entity.alive && other.alive && entity.x == other.x && entity.y == other.y {
                        out.entity_hits += 1;
                        out.digest = metric.mix(out.digest, entity.id ^ other.id);
                    }
                    j += 1;
                }
                i += 1;
            }
            out.valid = out.pairs == (world.entities.len() *
                (if world.entities.len() == 0 { 0 } else { world.entities.len() - 1 })) / 2;
            out
        }
    ''',
    "commands.rue": r'''
        // Input-to-command translation and stable validation.
        const model = @import("model.rue");

        pub fn enqueue_move(inout world: model.World, actor: u64, dx: i64, dy: i64) {
            let index = model.find_entity(borrow world, actor);
            let accepted = index != model.NONE() &&
                world.entities.get_or(index, model.empty_entity()).alive;
            world.commands.push(model.Command { tick: world.tick, kind: 1,
                actor: actor, x: dx, y: dy, value: 0, accepted: accepted });
        }

        pub fn enqueue_action(inout world: model.World, actor: u64, kind: u64, value: u64) {
            let index = model.find_entity(borrow world, actor);
            world.commands.push(model.Command { tick: world.tick, kind: kind,
                actor: actor, x: 0, y: 0, value: value,
                accepted: index != model.NONE() });
        }

        pub fn accepted(borrow world: model.World) -> u64 {
            let mut count: u64 = 0;
            let mut i: u64 = 0;
            while i < world.commands.len() {
                if world.commands.get_or(i, model.empty_command()).accepted { count += 1; }
                i += 1;
            }
            count
        }
    ''',
    "events.rue": r'''
        const metric = @import("metric.rue");
        const model = @import("model.rue");

        pub struct Audit { events: u64, monotonic: bool, digest: u64, valid: bool }

        pub fn audit(borrow world: model.World) -> Audit {
            let mut out = Audit { events: world.events.len(), monotonic: true,
                digest: 14695981039346656037, valid: false };
            let mut previous: u64 = 0;
            let mut i: u64 = 0;
            while i < world.events.len() {
                let event = world.events.get_or(i, model.empty_event());
                if i > 0 && event.sequence <= previous { out.monotonic = false; }
                previous = event.sequence;
                out.digest = metric.mix(out.digest, event.sequence);
                out.digest = metric.mix(out.digest, event.kind);
                out.digest = metric.mix(out.digest, event.actor);
                i += 1;
            }
            out.valid = out.monotonic && (out.events == 0 || previous < world.next_sequence);
            out
        }
    ''',
    "physics.rue": r'''
        // Deterministic integer-grid integration with blocked-tile response.
        const model = @import("model.rue");

        fn direction(id: u64, tick: u64, axis: u64) -> i64 {
            let mode = (id * 17 + tick * 13 + axis * 7) % 5;
            if mode == 0 { -1 } else if mode == 1 { 1 } else { 0 }
        }

        pub fn prepare(inout world: model.World) {
            let mut i: u64 = 0;
            while i < world.entities.len() {
                let entity = world.entities.get_or(i, model.empty_entity());
                if entity.alive {
                    world.entities.set(i, model.Entity { id: entity.id, kind: entity.kind,
                        faction: entity.faction, x: entity.x, y: entity.y,
                        previous_x: entity.x, previous_y: entity.y,
                        velocity_x: direction(entity.id, world.tick, 0),
                        velocity_y: direction(entity.id, world.tick, 1),
                        health: entity.health, energy: entity.energy, state: entity.state,
                        goal: entity.goal, inventory: entity.inventory, credits: entity.credits,
                        experience: entity.experience, born_tick: entity.born_tick,
                        updated_tick: world.tick, alive: entity.alive });
                }
                i += 1;
            }
        }

        pub fn integrate(inout world: model.World) -> u64 {
            let mut moved: u64 = 0;
            let mut i: u64 = 0;
            while i < world.entities.len() {
                let entity = world.entities.get_or(i, model.empty_entity());
                if entity.alive {
                    let nx = entity.x + entity.velocity_x;
                    let ny = entity.y + entity.velocity_y;
                    let allowed = model.walkable(borrow world, nx, ny);
                    let next_x = if allowed { nx } else { entity.x };
                    let next_y = if allowed { ny } else { entity.y };
                    if next_x != entity.x || next_y != entity.y { moved += 1; }
                    if !allowed { model.add_event(inout world, 2, entity.id, model.NONE(), 1); }
                    world.entities.set(i, model.Entity { id: entity.id, kind: entity.kind,
                        faction: entity.faction, x: next_x, y: next_y,
                        previous_x: entity.previous_x, previous_y: entity.previous_y,
                        velocity_x: entity.velocity_x, velocity_y: entity.velocity_y,
                        health: entity.health, energy: if entity.energy > 0 { entity.energy - 1 } else { 0 },
                        state: entity.state, goal: entity.goal, inventory: entity.inventory,
                        credits: entity.credits, experience: entity.experience + if allowed { 1 } else { 0 },
                        born_tick: entity.born_tick, updated_tick: world.tick, alive: entity.alive });
                }
                i += 1;
            }
            moved
        }

        pub fn step(inout world: model.World) -> u64 {
            prepare(inout world);
            integrate(inout world)
        }
    ''',
    "navigation.rue": r'''
        // Breadth-first grid navigation with stable neighbor ordering.
        const metric = @import("metric.rue");
        const model = @import("model.rue");

        pub struct Path { reached: bool, distance: u64, visited: u64,
            digest: u64, valid: bool }

        fn visit(borrow world: model.World, inout queue: model.U64s,
            inout distances: model.U64s, x: i64, y: i64, next_distance: u64) {
            if x < 0 || y < 0 { return; }
            let ux: u64 = @intCast(x);
            let uy: u64 = @intCast(y);
            let index = model.tile_index(borrow world, ux, uy);
            if index == model.NONE() || !world.tiles.get_or(index, model.empty_tile()).walkable { return; }
            if distances.get_or(index, model.NONE()) == model.NONE() {
                distances.set(index, next_distance);
                queue.push(index);
            }
        }

        pub fn find(borrow world: model.World, sx: u64, sy: u64, gx: u64, gy: u64) -> Path {
            let start = model.tile_index(borrow world, sx, sy);
            let goal = model.tile_index(borrow world, gx, gy);
            let mut out = Path { reached: false, distance: model.NONE(), visited: 0,
                digest: metric.mix(14695981039346656037, start), valid: false };
            if start == model.NONE() || goal == model.NONE() { return out; }
            let mut distances = model.U64s.new();
            let mut i: u64 = 0;
            while i < world.tiles.len() { distances.push(model.NONE()); i += 1; }
            let mut queue = model.U64s.new();
            distances.set(start, 0); queue.push(start);
            let mut head: u64 = 0;
            while head < queue.len() {
                let index = queue.get_or(head, model.NONE()); head += 1;
                let tile = world.tiles.get_or(index, model.empty_tile());
                let distance = distances.get_or(index, model.NONE());
                out.visited += 1;
                out.digest = metric.mix(out.digest, index);
                if index == goal { out.reached = true; out.distance = distance; break; }
                visit(borrow world, inout queue, inout distances,
                    @intCast(tile.x) + 1, @intCast(tile.y), distance + 1);
                visit(borrow world, inout queue, inout distances,
                    @intCast(tile.x), @intCast(tile.y) + 1, distance + 1);
                visit(borrow world, inout queue, inout distances,
                    @intCast(tile.x) - 1, @intCast(tile.y), distance + 1);
                visit(borrow world, inout queue, inout distances,
                    @intCast(tile.x), @intCast(tile.y) - 1, distance + 1);
            }
            out.valid = !out.reached || out.distance < world.tiles.len();
            out
        }
    ''',
    "navigation_oracle.rue": r'''
        // Independent relaxation-based shortest-path oracle.
        const model = @import("model.rue");
        const navigation = @import("navigation.rue");

        fn relax(borrow world: model.World, inout distances: model.U64s,
            from: u64, x: i64, y: i64) -> bool {
            if x < 0 || y < 0 { return false; }
            let target = model.tile_index(borrow world, @intCast(x), @intCast(y));
            if target == model.NONE() || !world.tiles.get_or(target, model.empty_tile()).walkable { return false; }
            let source_distance = distances.get_or(from, model.NONE());
            let target_distance = distances.get_or(target, model.NONE());
            if source_distance != model.NONE() &&
                (target_distance == model.NONE() || source_distance + 1 < target_distance) {
                distances.set(target, source_distance + 1); return true;
            }
            false
        }

        pub fn distance(borrow world: model.World, sx: u64, sy: u64, gx: u64, gy: u64) -> u64 {
            let start = model.tile_index(borrow world, sx, sy);
            let goal = model.tile_index(borrow world, gx, gy);
            if start == model.NONE() || goal == model.NONE() { return model.NONE(); }
            let mut distances = model.U64s.new();
            let mut i: u64 = 0;
            while i < world.tiles.len() { distances.push(model.NONE()); i += 1; }
            distances.set(start, 0);
            let mut pass: u64 = 0;
            while pass < world.tiles.len() {
                let mut changed = false;
                i = 0;
                while i < world.tiles.len() {
                    let tile = world.tiles.get_or(i, model.empty_tile());
                    if relax(borrow world, inout distances, i, @intCast(tile.x) + 1, @intCast(tile.y)) { changed = true; }
                    if relax(borrow world, inout distances, i, @intCast(tile.x), @intCast(tile.y) + 1) { changed = true; }
                    if relax(borrow world, inout distances, i, @intCast(tile.x) - 1, @intCast(tile.y)) { changed = true; }
                    if relax(borrow world, inout distances, i, @intCast(tile.x), @intCast(tile.y) - 1) { changed = true; }
                    i += 1;
                }
                if !changed { break; }
                pass += 1;
            }
            distances.get_or(goal, model.NONE())
        }

        pub fn agrees(borrow world: model.World, sx: u64, sy: u64, gx: u64, gy: u64,
            borrow actual: navigation.Path) -> bool {
            let expected = distance(borrow world, sx, sy, gx, gy);
            (actual.reached && actual.distance == expected) ||
                (!actual.reached && expected == model.NONE())
        }
    ''',
})

CORE.update({
    "workload.rue": r'''
        const metric = @import("metric.rue");
        const std = @import("std");
        const stress = @import("stress.rue");
        const text = @import("text.rue");
        const StrBuf = std.strbuf.StrBuf;
        pub struct Result { tiers: u64, entities: u64, ticks: u64,
            digest: u64, valid: bool }
        pub fn run() -> Result {
            let one = stress.run(1); let two = stress.run(2);
            Result { tiers: 2, entities: one.entities + two.entities,
                ticks: one.ticks + two.ticks, digest: metric.mix(one.digest, two.digest),
                valid: one.valid && two.valid && two.entities == one.entities * 2 &&
                    two.ticks == one.ticks * 2 }
        }
        pub fn render(borrow value: Result) -> StrBuf {
            let mut out = StrBuf.new(); text.line(inout out, "benchmark tiers=", value.tiers);
            text.line(inout out, "entities=", value.entities); text.line(inout out, "ticks=", value.ticks);
            text.line(inout out, "digest=", value.digest); text.bool_line(inout out, "valid=", value.valid); out
        }
    ''',
    "main.rue": r'''
        // caldera — deterministic simulation, scripting, rollback, and content lab.
        const diagnostics = @import("diagnostics.rue");
        const lexer = @import("lexer.rue");
        const parser = @import("parser.rue");
        const report = @import("report.rue");
        const scenario_source = @import("scenario_source.rue");
        const script_lower = @import("script_lower.rue");
        const script_oracle = @import("script_oracle.rue");
        const selftest = @import("selftest.rue");
        const simulation = @import("simulation.rue");
        const std = @import("std");
        const stress = @import("stress.rue");
        const text = @import("text.rue");
        const workload = @import("workload.rue");
        const worldgen = @import("worldgen.rue");
        const StrBuf = std.strbuf.StrBuf;
        const Bytes = std.arraybuf.ArrayBuf(u8);
        const OptStr = std.option.Option(StrBuf);
        const OptBytes = std.option.Option(Bytes);
        const FileRes = std.result.Result(std.fs.File, std.fs.FileError);
        const ReadRes = std.result.Result(u64, std.fs.FileError);

        fn same(borrow value: StrBuf, expected: StrBuf) -> bool {
            StrBuf.equals_borrowed(borrow value, borrow expected)
        }

        fn read_file(borrow path: StrBuf) -> OptBytes {
            let O = OptBytes; let mut data = Bytes.new();
            let mut file = match std.fs.File.open(borrow path) {
                FileRes.Ok(value) => value,
                FileRes.Err(error) => { return O.None; },
            };
            loop {
                data.reserve(65536);
                let count = match file.read(inout data) {
                    ReadRes.Ok(value) => value,
                    ReadRes.Err(error) => { return O.None; },
                };
                if count == 0 { break; }
            }
            let _ = file.close(); O.Some(data)
        }

        fn execute(source: Bytes) -> i32 {
            let scanned = lexer.scan(source);
            if scanned.errors > 0 { println("caldera[syntax]: unrecognized token"); return 1; }
            let scenario = parser.parse(scanned.tokens);
            if scenario.errors > 0 { println("caldera[syntax]: malformed scenario"); return 1; }
            if !scenario.valid {
                println("caldera[semantic]: EXPECT ENTITIES does not match declarations"); return 1;
            }
            let program = script_lower.lower(borrow scenario);
            let script = script_oracle.verify(borrow scenario, borrow program);
            if !script.valid { println("caldera[script]: VM and interpreter disagree"); return 1; }
            let mut world = worldgen.from_scenario(borrow scenario);
            if world.diagnostics.len() > 0 { print(diagnostics.render(borrow world)); return 1; }
            world.script_steps = script.vm_steps;
            let run = simulation.run(inout world, scenario.ticks);
            let mut out = report.summary(borrow world, borrow run);
            text.line(inout out, "script_steps=", script.vm_steps);
            text.bool_line(inout out, "script_oracle=", script.valid);
            print(out); if run.valid && script.valid { 0 } else { 1 }
        }

        fn demo() -> i32 {
            let mut world = worldgen.demo(1);
            let run = simulation.run(inout world, 4);
            print(report.summary(borrow world, borrow run));
            if run.valid { 0 } else { 1 }
        }

        fn help() -> StrBuf {
            let mut out = StrBuf.new();
            text.append_literal(inout out, "usage: caldera [demo | run FILE | selftest | stress1 | stress2 | stress4 | benchmark | help]\n");
            text.append_literal(inout out, "Caldera simulates deterministic worlds, scripts, rollback, replay, and content systems.\n"); out
        }

        fn run_selftest() -> i32 {
            let value = selftest.run(); print(selftest.render(borrow value));
            if value.valid { 0 } else { 1 }
        }

        fn main() -> i32 {
            let command = match std.env.arg(1) {
                OptStr.Some(value) => value,
                OptStr.None => { return run_selftest(); },
            };
            if same(borrow command, "demo") { return demo(); }
            if same(borrow command, "selftest") { return run_selftest(); }
            if same(borrow command, "stress1") || same(borrow command, "stress2") ||
                same(borrow command, "stress4") {
                let scale: u64 = if same(borrow command, "stress1") { 1 }
                    else if same(borrow command, "stress2") { 2 } else { 4 };
                let value = stress.run(scale); print(stress.render(borrow value));
                return if value.valid { 0 } else { 1 };
            }
            if same(borrow command, "benchmark") {
                let value = workload.run(); print(workload.render(borrow value));
                return if value.valid { 0 } else { 1 };
            }
            if same(borrow command, "help") || same(borrow command, "--help") {
                print(help()); return 0;
            }
            if same(borrow command, "run") {
                return match std.env.arg(2) {
                    OptStr.Some(path) => match read_file(borrow path) {
                        OptBytes.Some(bytes) => execute(bytes),
                        OptBytes.None => { println("caldera[filesystem]: cannot read scenario"); 2 },
                    },
                    OptStr.None => { println("caldera: run requires a scenario path"); 2 },
                };
            }
            println("caldera: command is not implemented yet"); 2
        }
    ''',
    "canary.rue": r'''
        // Reduced pre-merge canary for Caldera's core simulation path. The scheduled
        // slow target compiles the complete generated application graph.
        const checksum = @import("checksum.rue");
        const simulation = @import("simulation.rue");
        const worldgen = @import("worldgen.rue");

        fn main() -> i32 {
            let mut world = worldgen.demo(1);
            let before = checksum.world(borrow world);
            let result = simulation.run(inout world, 4);
            let after = checksum.world(borrow world);
            if result.valid && result.ticks == 4 && before != after && after == result.checksum {
                0
            } else {
                1
            }
        }
    ''',
})

CORE.update({
    "selftest.rue": r'''
        // Cross-oracle verification for every Caldera subsystem and generated family.
        const ai_oracle = @import("ai_oracle.rue");
        const assets = @import("assets.rue");
        const audio_commands = @import("audio_commands.rue");
        const audit_suite = @import("audit_suite.rue");
        const behavior_suite = @import("behavior_suite.rue");
        const checksum = @import("checksum.rue");
        const crafting = @import("crafting.rue");
        const delta = @import("delta.rue");
        const desync = @import("desync.rue");
        const dialogue = @import("dialogue.rue");
        const events = @import("events.rue");
        const factions = @import("factions.rue");
        const inventory = @import("inventory.rue");
        const lexer = @import("lexer.rue");
        const localization = @import("localization.rue");
        const lockstep = @import("lockstep.rue");
        const metric = @import("metric.rue");
        const model = @import("model.rue");
        const navigation = @import("navigation.rue");
        const navigation_oracle = @import("navigation_oracle.rue");
        const parser = @import("parser.rue");
        const persistence = @import("persistence.rue");
        const prediction = @import("prediction.rue");
        const quests = @import("quests.rue");
        const render_commands = @import("render_commands.rue");
        const replay = @import("replay.rue");
        const replay_oracle = @import("replay_oracle.rue");
        const report_suite = @import("report_suite.rue");
        const rollback = @import("rollback.rue");
        const rule_suite = @import("rule_suite.rue");
        const scenario_source = @import("scenario_source.rue");
        const scheduler = @import("scheduler.rue");
        const scheduler_oracle = @import("scheduler_oracle.rue");
        const script_disasm = @import("script_disasm.rue");
        const script_lower = @import("script_lower.rue");
        const script_oracle = @import("script_oracle.rue");
        const simulation = @import("simulation.rue");
        const snapshot = @import("snapshot.rue");
        const spatial = @import("spatial.rue");
        const std = @import("std");
        const system_suite = @import("system_suite.rue");
        const text = @import("text.rue");
        const world_clone = @import("world_clone.rue");
        const worldgen = @import("worldgen.rue");
        const StrBuf = std.strbuf.StrBuf;

        pub struct Result {
            checks: u64, failures: u64, entities: u64, ticks: u64,
            audit_passes: u64, systems: u64, behaviors: u64, rules: u64,
            report_documents: u64, vm_steps: u64, oracle_mismatches: u64,
            deterministic_mismatches: u64, digest: u64, valid: bool,
        }

        fn check(inout out: Result, condition: bool, fingerprint: u64) {
            out.checks += 1; out.digest = metric.mix(out.digest, fingerprint);
            if !condition { out.failures += 1; }
        }

        pub fn run() -> Result {
            let baseline = worldgen.demo(1);
            let mut out = Result { checks: 0, failures: 0, entities: baseline.entities.len(),
                ticks: 0, audit_passes: 0, systems: 0, behaviors: 0, rules: 0,
                report_documents: 0, vm_steps: 0, oracle_mismatches: 0,
                deterministic_mismatches: 0, digest: 14695981039346656037, valid: false };

            let schedule = scheduler.build(borrow baseline);
            check(inout out, schedule.valid && scheduler_oracle.agrees(borrow baseline, borrow schedule), schedule.digest);
            check(inout out, ai_oracle.verify(borrow baseline), baseline.entities.len());
            let first = baseline.entities.get_or(0, model.empty_entity());
            let path = navigation.find(borrow baseline, @intCast(first.x), @intCast(first.y),
                @intCast(first.x), @intCast(first.y));
            check(inout out, path.valid && navigation_oracle.agrees(borrow baseline,
                @intCast(first.x), @intCast(first.y), @intCast(first.x), @intCast(first.y), borrow path), path.digest);
            let nearby = spatial.region(borrow baseline, first.x, first.y, 4);
            check(inout out, nearby.valid && nearby.matches > 0 &&
                spatial.nearest(borrow baseline, first.x, first.y) == first.id, nearby.digest);

            let scanned = lexer.scan(scenario_source.demo());
            check(inout out, scanned.errors == 0, scanned.tokens.len());
            let scenario = parser.parse(scanned.tokens);
            check(inout out, scenario.valid && scenario.entities.len() == 4, scenario.ticks);
            let program = script_lower.lower(borrow scenario);
            let agreement = script_oracle.verify(borrow scenario, borrow program);
            out.vm_steps = agreement.vm_steps;
            check(inout out, agreement.valid, agreement.vm_value);
            let disassembly = script_disasm.render(borrow program);
            check(inout out, disassembly.len() > 32 && text.fnv1a(borrow disassembly) != 0, disassembly.len());

            let mut world = world_clone.clone(borrow baseline);
            let run_result = simulation.run(inout world, 4);
            out.ticks = run_result.ticks;
            check(inout out, run_result.valid && world.tick == 4, run_result.checksum);
            let replayed = replay.run(borrow baseline, 4);
            let replay_ok = replay_oracle.agrees(borrow baseline, 4, borrow replayed);
            if !replay_ok { out.oracle_mismatches += 1; }
            check(inout out, replay_ok && replayed.final_checksum == checksum.world(borrow world), replayed.timeline_digest);
            check(inout out, rollback.agrees(borrow baseline, 4, checksum.world(borrow world)), replayed.final_checksum);
            let synchronized = lockstep.run(borrow baseline, 4);
            check(inout out, synchronized.valid && synchronized.mismatches == 0, synchronized.checksum);
            let predicted = prediction.run(borrow baseline, 4);
            check(inout out, predicted.valid && predicted.predicted_checksum == replayed.final_checksum, predicted.confidence);

            let copy = world_clone.clone(borrow world);
            let difference = delta.compare(borrow world, borrow copy);
            let synchronization = desync.compare(borrow world, borrow copy);
            check(inout out, difference.valid && difference.before == difference.after, difference.digest);
            check(inout out, synchronization.valid && !synchronization.mismatch, synchronization.digest);
            let timeline = events.audit(borrow world);
            let snapshots = snapshot.audit(borrow world);
            check(inout out, timeline.valid && snapshots.valid && snapshots.snapshots == 4,
                metric.mix(timeline.digest, snapshots.digest));

            let inventory_audit = inventory.audit(borrow world);
            let craft_audit = crafting.audit(borrow world);
            let relations = factions.audit(borrow world);
            let quest_audit = quests.audit(borrow world);
            let dialogue_audit = dialogue.audit(borrow world);
            check(inout out, metric.valid(borrow inventory_audit) && craft_audit.valid &&
                relations.valid && quest_audit.valid && dialogue_audit.valid,
                metric.mix(inventory_audit.digest, craft_audit.digest));
            let asset_audit = assets.audit(borrow world);
            let locale_audit = localization.audit(borrow world);
            let render_frame = render_commands.build(borrow world);
            let audio_frame = audio_commands.build(borrow world);
            let saved = persistence.save(borrow world);
            check(inout out, asset_audit.valid && locale_audit.valid && render_frame.valid &&
                audio_frame.valid && persistence.verify(borrow world, borrow saved),
                metric.mix(asset_audit.digest, locale_audit.digest));

            let audits = audit_suite.run(borrow world); out.audit_passes = audits.passes;
            check(inout out, audits.valid && audits.passes == 256, audits.digest);
            let systems = system_suite.run(borrow world); out.systems = systems.systems;
            check(inout out, systems.valid && systems.systems == 192, systems.digest);
            let behaviors = behavior_suite.run(borrow world); out.behaviors = behaviors.behaviors;
            check(inout out, behaviors.valid && behaviors.behaviors == 128, behaviors.digest);
            let rules = rule_suite.run(borrow world); out.rules = rules.rules;
            check(inout out, rules.valid && rules.rules == 96, rules.digest);
            let suite_digest = metric.mix(metric.mix(audits.digest, systems.digest),
                metric.mix(behaviors.digest, rules.digest));
            let reports = report_suite.run(borrow world, suite_digest);
            out.report_documents = reports.documents;
            check(inout out, reports.valid && reports.documents == 64, reports.digest);
            out.deterministic_mismatches = audits.deterministic_mismatches +
                systems.deterministic_mismatches + behaviors.deterministic_mismatches +
                rules.deterministic_mismatches + reports.deterministic_mismatches;
            out.valid = out.failures == 0 && out.oracle_mismatches == 0 &&
                out.deterministic_mismatches == 0 && out.audit_passes == 256 &&
                out.systems == 192 && out.behaviors == 128 && out.rules == 96 &&
                out.report_documents == 64; out
        }

        pub fn render(borrow value: Result) -> StrBuf {
            let mut out = StrBuf.new();
            text.line(inout out, "selftest checks=", value.checks);
            text.line(inout out, "failures=", value.failures);
            text.line(inout out, "entities=", value.entities);
            text.line(inout out, "ticks=", value.ticks);
            text.line(inout out, "audit_passes=", value.audit_passes);
            text.line(inout out, "systems=", value.systems);
            text.line(inout out, "behaviors=", value.behaviors);
            text.line(inout out, "rules=", value.rules);
            text.line(inout out, "report_documents=", value.report_documents);
            text.line(inout out, "vm_steps=", value.vm_steps);
            text.line(inout out, "oracle_mismatches=", value.oracle_mismatches);
            text.line(inout out, "deterministic_mismatches=", value.deterministic_mismatches);
            text.line(inout out, "digest=", value.digest);
            text.bool_line(inout out, "valid=", value.valid); out
        }
    ''',
    "stress.rue": r'''
        // Complete scaling tier executes every generated family and oracle twice.
        const audit_suite = @import("audit_suite.rue");
        const behavior_suite = @import("behavior_suite.rue");
        const checksum = @import("checksum.rue");
        const metric = @import("metric.rue");
        const navigation = @import("navigation.rue");
        const navigation_oracle = @import("navigation_oracle.rue");
        const replay = @import("replay.rue");
        const replay_oracle = @import("replay_oracle.rue");
        const report_suite = @import("report_suite.rue");
        const rule_suite = @import("rule_suite.rue");
        const simulation = @import("simulation.rue");
        const std = @import("std");
        const system_suite = @import("system_suite.rue");
        const text = @import("text.rue");
        const world_clone = @import("world_clone.rue");
        const worldgen = @import("worldgen.rue");
        const StrBuf = std.strbuf.StrBuf;

        pub struct Result { scale: u64, entities: u64, tiles: u64, ticks: u64,
            events: u64, audit_passes: u64, systems: u64, behaviors: u64,
            rules: u64, report_documents: u64, oracle_mismatches: u64,
            deterministic_mismatches: u64, digest: u64, valid: bool }

        fn once(scale: u64) -> Result {
            let baseline = worldgen.demo(scale);
            let mut world = world_clone.clone(borrow baseline);
            let run = simulation.run(inout world, scale * 2);
            let audits = audit_suite.run(borrow world);
            let systems = system_suite.run(borrow world);
            let behaviors = behavior_suite.run(borrow world);
            let rules = rule_suite.run(borrow world);
            let suite_digest = metric.mix(metric.mix(audits.digest, systems.digest),
                metric.mix(behaviors.digest, rules.digest));
            let reports = report_suite.run(borrow world, suite_digest);
            let path = navigation.find(borrow world, 0, 0, world.width - 1, world.height - 1);
            let path_ok = navigation_oracle.agrees(borrow world, 0, 0,
                world.width - 1, world.height - 1, borrow path);
            let replayed = replay.run(borrow baseline, scale * 2);
            let replay_ok = replay_oracle.agrees(borrow baseline, scale * 2, borrow replayed) &&
                replayed.final_checksum == checksum.world(borrow world);
            let oracle_mismatches = if path_ok && replay_ok { 0 } else { 1 };
            let deterministic = audits.deterministic_mismatches + systems.deterministic_mismatches +
                behaviors.deterministic_mismatches + rules.deterministic_mismatches +
                reports.deterministic_mismatches;
            let digest = metric.mix(metric.mix(suite_digest, reports.digest),
                metric.mix(run.checksum, replayed.timeline_digest));
            Result { scale: scale, entities: world.entities.len(), tiles: world.tiles.len(),
                ticks: run.ticks, events: world.events.len(), audit_passes: audits.passes,
                systems: systems.systems, behaviors: behaviors.behaviors, rules: rules.rules,
                report_documents: reports.documents, oracle_mismatches: oracle_mismatches,
                deterministic_mismatches: deterministic, digest: digest,
                valid: run.valid && audits.valid && systems.valid && behaviors.valid &&
                    rules.valid && reports.valid && oracle_mismatches == 0 && deterministic == 0 }
        }

        pub fn run(scale: u64) -> Result {
            let first = once(scale); let second = once(scale);
            let mismatch = if first.digest == second.digest && first.entities == second.entities &&
                first.events == second.events && first.ticks == second.ticks { 0 } else { 1 };
            Result { scale: first.scale, entities: first.entities, tiles: first.tiles,
                ticks: first.ticks, events: first.events, audit_passes: first.audit_passes,
                systems: first.systems, behaviors: first.behaviors, rules: first.rules,
                report_documents: first.report_documents,
                oracle_mismatches: first.oracle_mismatches,
                deterministic_mismatches: first.deterministic_mismatches + mismatch,
                digest: first.digest, valid: first.valid && second.valid && mismatch == 0 }
        }

        pub fn render(borrow value: Result) -> StrBuf {
            let mut out = StrBuf.new(); text.line(inout out, "stress scale=", value.scale);
            text.line(inout out, "entities=", value.entities); text.line(inout out, "tiles=", value.tiles);
            text.line(inout out, "ticks=", value.ticks); text.line(inout out, "events=", value.events);
            text.line(inout out, "audit_passes=", value.audit_passes);
            text.line(inout out, "systems=", value.systems); text.line(inout out, "behaviors=", value.behaviors);
            text.line(inout out, "rules=", value.rules); text.line(inout out, "report_documents=", value.report_documents);
            text.line(inout out, "oracle_mismatches=", value.oracle_mismatches);
            text.line(inout out, "deterministic_mismatches=", value.deterministic_mismatches);
            text.line(inout out, "digest=", value.digest); text.bool_line(inout out, "valid=", value.valid); out
        }
    ''',
})

CORE.update({
    "scenario_source.rue": r'''
        const std = @import("std");
        pub const Bytes = std.arraybuf.ArrayBuf(u8);
        pub fn bytes(value: str) -> Bytes {
            let mut out = Bytes.new(); let mut i: u64 = 0;
            while i < value.len() { out.push(value[i]); i += 1; }
            out
        }
        pub fn demo() -> Bytes {
            bytes("WORLD 10 10; ENTITY WORKER 1 1 1; ENTITY SCOUT 1 2 2; ENTITY BUILDER 2 3 3; ENTITY GUARD 2 4 4; TILE 5 5 WALL; TILE 6 5 FOREST; TICK 4; EXPECT ENTITIES 4;")
        }
    ''',
    "assets.rue": r'''
        const metric = @import("metric.rue");
        const model = @import("model.rue");
        pub struct Audit { assets: u64, referenced: u64, missing: u64,
            digest: u64, valid: bool }
        pub fn audit(borrow world: model.World) -> Audit {
            let assets = 48 + world.entities.len() * 3;
            let referenced = world.entities.len() + 12;
            Audit { assets: assets, referenced: referenced, missing: 0,
                digest: metric.mix(metric.mix(world.seed, assets), referenced),
                valid: referenced <= assets }
        }
    ''',
    "localization.rue": r'''
        const metric = @import("metric.rue");
        const model = @import("model.rue");
        pub struct Audit { messages: u64, locales: u64, missing: u64,
            digest: u64, valid: bool }
        pub fn audit(borrow world: model.World) -> Audit {
            let messages = 96 + world.quests_completed * 2;
            let locales = 5;
            Audit { messages: messages, locales: locales, missing: 0,
                digest: metric.mix(metric.mix(world.seed, messages), locales), valid: messages > locales }
        }
    ''',
    "render_commands.rue": r'''
        const metric = @import("metric.rue");
        const model = @import("model.rue");
        pub struct Frame { sprites: u64, tiles: u64, overlays: u64,
            batches: u64, digest: u64, valid: bool }
        pub fn build(borrow world: model.World) -> Frame {
            let sprites = model.alive_count(borrow world);
            let tiles = world.tiles.len();
            let overlays = world.events.len() + world.diagnostics.len();
            let batches = (sprites + tiles + overlays + 15) / 16;
            Frame { sprites: sprites, tiles: tiles, overlays: overlays, batches: batches,
                digest: metric.mix(metric.mix(world.seed, sprites + tiles), overlays),
                valid: batches > 0 && sprites <= world.entities.len() }
        }
    ''',
    "audio_commands.rue": r'''
        const metric = @import("metric.rue");
        const model = @import("model.rue");
        pub struct Frame { emitters: u64, cues: u64, voices: u64,
            digest: u64, valid: bool }
        pub fn build(borrow world: model.World) -> Frame {
            let emitters = model.alive_count(borrow world);
            let cues = world.events.len();
            let voices = if cues < 32 { cues } else { 32 };
            Frame { emitters: emitters, cues: cues, voices: voices,
                digest: metric.mix(metric.mix(world.seed, emitters), cues), valid: voices <= cues }
        }
    ''',
    "persistence.rue": r'''
        const checksum = @import("checksum.rue");
        const metric = @import("metric.rue");
        const model = @import("model.rue");
        pub struct Save { version: u64, tick: u64, entities: u64, tiles: u64,
            checksum: u64, bytes: u64, valid: bool }
        pub fn save(borrow world: model.World) -> Save {
            let size = 64 + world.entities.len() * 152 + world.tiles.len() * 72 +
                world.events.len() * 48 + world.snapshots.len() * 40;
            Save { version: 1, tick: world.tick, entities: world.entities.len(),
                tiles: world.tiles.len(), checksum: checksum.world(borrow world),
                bytes: size, valid: world.valid && size > 64 }
        }
        pub fn verify(borrow world: model.World, borrow value: Save) -> bool {
            value.valid && value.version == 1 && value.tick == world.tick &&
                value.entities == world.entities.len() && value.tiles == world.tiles.len() &&
                value.checksum == checksum.world(borrow world) &&
                metric.mix(value.checksum, value.bytes) != 0
        }
    ''',
    "diagnostics.rue": r'''
        const model = @import("model.rue");
        const std = @import("std");
        const text = @import("text.rue");
        const StrBuf = std.strbuf.StrBuf;
        pub fn render(borrow world: model.World) -> StrBuf {
            let mut out = StrBuf.new(); let mut i: u64 = 0;
            while i < world.diagnostics.len() {
                let item = world.diagnostics.get_or(i, model.Diagnostic { code: 0, subject: 0, detail: 0 });
                text.append_literal(inout out, "caldera[world] code="); text.append_u64(inout out, item.code);
                text.append_literal(inout out, " subject="); text.append_u64(inout out, item.subject);
                text.append_literal(inout out, " detail="); text.append_u64(inout out, item.detail); out.push(10);
                i += 1;
            }
            out
        }
    ''',
    "report.rue": r'''
        const checksum = @import("checksum.rue");
        const model = @import("model.rue");
        const simulation = @import("simulation.rue");
        const std = @import("std");
        const text = @import("text.rue");
        const StrBuf = std.strbuf.StrBuf;
        pub fn summary(borrow world: model.World, borrow run: simulation.Result) -> StrBuf {
            let mut out = StrBuf.new();
            text.append_literal(inout out, "engine=caldera\n");
            text.line(inout out, "tick=", world.tick);
            text.line(inout out, "width=", world.width);
            text.line(inout out, "height=", world.height);
            text.line(inout out, "entities=", world.entities.len());
            text.line(inout out, "alive=", model.alive_count(borrow world));
            text.line(inout out, "tiles=", world.tiles.len());
            text.line(inout out, "events=", world.events.len());
            text.line(inout out, "snapshots=", world.snapshots.len());
            text.line(inout out, "moved=", run.moved);
            text.line(inout out, "ai_changes=", run.ai_changes);
            text.line(inout out, "produced=", run.produced);
            text.line(inout out, "collisions=", run.collisions);
            text.line(inout out, "resources=", world.resources);
            text.line(inout out, "treasury=", world.treasury);
            text.line(inout out, "checksum=", checksum.world(borrow world));
            text.bool_line(inout out, "valid=", run.valid && world.valid); out
        }
    ''',
})

CORE.update({
    "system_result.rue": r'''
        const metric = @import("metric.rue");
        pub struct Result { system: u64, inspected: u64, eligible: u64,
            commands: u64, energy: u64, digest: u64, valid: bool }
        pub fn new(system: u64) -> Result {
            Result { system: system, inspected: 0, eligible: 0, commands: 0,
                energy: 0, digest: metric.mix(14695981039346656037, system), valid: false }
        }
        pub fn record(inout out: Result, eligible: bool, command: bool, value: u64) {
            out.inspected += 1;
            if eligible { out.eligible += 1; }
            if command { out.commands += 1; }
            out.energy = @wrapping_add(out.energy, value);
            out.digest = metric.mix(out.digest, value + out.inspected * 17);
        }
        pub fn finish(inout out: Result) {
            out.digest = metric.mix(out.digest, out.eligible);
            out.digest = metric.mix(out.digest, out.commands);
            out.valid = out.inspected > 0 && out.commands <= out.eligible &&
                out.eligible <= out.inspected;
        }
    ''',
    "behavior_result.rue": r'''
        const metric = @import("metric.rue");
        pub struct Result { behavior: u64, agents: u64, selected: u64,
            utility: u64, transitions: u64, digest: u64, valid: bool }
        pub fn new(behavior: u64) -> Result {
            Result { behavior: behavior, agents: 0, selected: 0, utility: 0,
                transitions: 0, digest: metric.mix(14695981039346656037, behavior), valid: false }
        }
        pub fn record(inout out: Result, selected: bool, utility: u64, transition: bool) {
            out.agents += 1;
            if selected { out.selected += 1; }
            if transition { out.transitions += 1; }
            out.utility = @wrapping_add(out.utility, utility);
            out.digest = metric.mix(out.digest, utility + out.agents * 31);
        }
        pub fn finish(inout out: Result) {
            out.digest = metric.mix(out.digest, out.selected);
            out.digest = metric.mix(out.digest, out.transitions);
            out.valid = out.agents > 0 && out.selected <= out.agents &&
                out.transitions <= out.agents;
        }
    ''',
    "rule_result.rue": r'''
        const metric = @import("metric.rue");
        pub struct Result { rule: u64, inspected: u64, matches: u64,
            actions: u64, benefit: u64, digest: u64, valid: bool }
        pub fn new(rule: u64) -> Result {
            Result { rule: rule, inspected: 0, matches: 0, actions: 0,
                benefit: 0, digest: metric.mix(14695981039346656037, rule), valid: false }
        }
        pub fn record(inout out: Result, matched: bool, acted: bool, benefit: u64) {
            out.inspected += 1;
            if matched { out.matches += 1; }
            if acted { out.actions += 1; }
            out.benefit = @wrapping_add(out.benefit, benefit);
            out.digest = metric.mix(out.digest, benefit + out.inspected * 43);
        }
        pub fn finish(inout out: Result) {
            out.digest = metric.mix(out.digest, out.matches);
            out.digest = metric.mix(out.digest, out.actions);
            out.valid = out.inspected > 0 && out.actions <= out.matches &&
                out.matches <= out.inspected;
        }
    ''',
})


AUDIT_DOMAINS = [
    "world", "entity", "component", "terrain", "spatial", "physics", "navigation", "ai",
    "economy", "inventory", "faction", "quest", "script", "network", "replay", "render",
]
AUDIT_CONCERNS = [
    "density", "balance", "pressure", "locality", "fanout", "continuity", "coverage", "skew",
    "capacity", "latency", "stability", "integrity", "cost", "risk", "throughput", "entropy",
]
SYSTEM_DOMAINS = [
    "lifecycle", "movement", "collision", "visibility", "perception", "decision",
    "economy", "production", "logistics", "combat", "quest", "network",
]
SYSTEM_ACTIONS = [
    "bootstrap", "discover", "validate", "schedule", "prepare", "dispatch", "apply", "resolve",
    "reconcile", "compact", "checkpoint", "replicate", "recover", "observe", "finalize", "audit",
]
BEHAVIOR_ROLES = ["worker", "scout", "builder", "guard", "trader", "healer", "leader", "citizen"]
BEHAVIOR_INTENTS = [
    "idle", "explore", "gather", "deliver", "craft", "construct", "repair", "defend",
    "pursue", "evade", "assist", "trade", "rest", "learn", "coordinate", "celebrate",
]
RULE_FAMILIES = ["terrain", "spawn", "resource", "combat", "economy", "quest", "network", "render"]
RULE_POLICIES = [
    "normalize", "bound", "merge", "split", "prioritize", "defer",
    "reserve", "release", "replicate", "recover", "validate", "explain",
]
REPORT_CHANNELS = ["world", "entity", "physics", "ai", "economy", "script", "network", "replay"]
REPORT_FORMATS = ["human", "json", "csv", "ndjson", "markdown", "prometheus", "trace", "graph"]


def audit_names() -> list[str]:
    return [f"audit_{domain}_{concern}" for domain in AUDIT_DOMAINS for concern in AUDIT_CONCERNS]


def system_names() -> list[str]:
    return [f"system_{domain}_{action}" for domain in SYSTEM_DOMAINS for action in SYSTEM_ACTIONS]


def behavior_names() -> list[str]:
    return [f"behavior_{role}_{intent}" for role in BEHAVIOR_ROLES for intent in BEHAVIOR_INTENTS]


def rule_names() -> list[str]:
    return [f"rule_{family}_{policy}" for family in RULE_FAMILIES for policy in RULE_POLICIES]


def report_names() -> list[str]:
    return [f"report_{channel}_{fmt}" for channel in REPORT_CHANNELS for fmt in REPORT_FORMATS]


def emit_audits() -> None:
    template = r'''
        // Caldera audit pass: __LABEL__.
        // Every pass traverses the canonical world while its mode selects a
        // distinct projection and weighting policy.
        const metric = @import("metric.rue");
        const model = @import("model.rue");
        const std = @import("std");
        const text = @import("text.rue");
        const StrBuf = std.strbuf.StrBuf;

        pub fn TAG() -> u64 { __TAG__ }
        pub fn MODE() -> u64 { __MODE__ }

        fn entity_projection(borrow world: model.World, index: u64) -> u64 {
            let item = world.entities.get_or(index, model.empty_entity());
            let position: u64 = @intCast(item.x + item.y + 8192);
            let motion: u64 = @intCast(item.velocity_x + item.velocity_y + 128);
            let base = if MODE() % 8 == 0 { item.health + item.energy }
                else if MODE() % 8 == 1 { position * (item.kind + 1) }
                else if MODE() % 8 == 2 { item.inventory + item.credits * 3 }
                else if MODE() % 8 == 3 { item.experience + item.updated_tick * 5 }
                else if MODE() % 8 == 4 { motion + item.state * 11 }
                else if MODE() % 8 == 5 { item.faction * 17 + item.kind * 23 }
                else if MODE() % 8 == 6 { item.born_tick + item.goal % 1009 }
                else { item.id * (MODE() + 1) + if item.alive { 7 } else { 0 } };
            base + MODE()
        }

        fn tile_projection(borrow world: model.World, index: u64) -> u64 {
            let item = world.tiles.get_or(index, model.empty_tile());
            let flags = (if item.walkable { 3 } else { 11 }) +
                (if item.visible { 5 } else { 0 });
            if MODE() % 6 == 0 { item.x + item.y * world.width + flags }
            else if MODE() % 6 == 1 { item.terrain * 31 + item.height * 7 + flags }
            else if MODE() % 6 == 2 { item.resource * (MODE() + 1) + item.region }
            else if MODE() % 6 == 3 { index + item.height + item.resource + flags }
            else if MODE() % 6 == 4 { (item.x + 1) * (item.y + 1) + item.terrain }
            else { item.region * 19 + item.occupant % 1021 + flags }
        }

        fn event_projection(borrow world: model.World, index: u64) -> u64 {
            let item = world.events.get_or(index, model.empty_event());
            item.tick * (MODE() % 7 + 1) + item.kind * 13 + item.actor % 997 +
                item.target % 991 + item.value + item.sequence
        }

        fn command_projection(borrow world: model.World, index: u64) -> u64 {
            let item = world.commands.get_or(index, model.empty_command());
            let position: u64 = @intCast(item.x + item.y + 256);
            item.tick + item.kind * 17 + item.actor % 983 + position + item.value +
                if item.accepted { MODE() + 1 } else { 0 }
        }

        fn snapshot_projection(borrow world: model.World, index: u64) -> u64 {
            let item = world.snapshots.get_or(index, model.empty_snapshot());
            item.tick + item.entity_count * 3 + item.event_count * 5 +
                item.command_count * 7 + item.checksum % 65521 + MODE()
        }

        fn observe_entities(inout out: metric.Metric, borrow world: model.World) {
            let mut i: u64 = 0;
            while i < world.entities.len() {
                let item = world.entities.get_or(i, model.empty_entity());
                metric.observe(inout out, entity_projection(borrow world, i), (i % 11) + 1);
                metric.warn(inout out, item.alive && (item.x < 0 || item.y < 0 ||
                    @intCast(item.x) >= world.width || @intCast(item.y) >= world.height));
                i += 1;
            }
        }

        fn observe_tiles(inout out: metric.Metric, borrow world: model.World) {
            let mut i: u64 = 0;
            while i < world.tiles.len() {
                let item = world.tiles.get_or(i, model.empty_tile());
                metric.observe(inout out, tile_projection(borrow world, i), (i % 13) + 1);
                metric.warn(inout out, item.x >= world.width || item.y >= world.height);
                i += 1;
            }
        }

        fn observe_timeline(inout out: metric.Metric, borrow world: model.World) {
            let mut i: u64 = 0;
            while i < world.events.len() {
                metric.observe(inout out, event_projection(borrow world, i), (i % 5) + 1);
                metric.warn(inout out, world.events.get_or(i, model.empty_event()).tick > world.tick);
                i += 1;
            }
            i = 0;
            while i < world.commands.len() {
                metric.observe(inout out, command_projection(borrow world, i), (i % 7) + 1);
                metric.warn(inout out, world.commands.get_or(i, model.empty_command()).tick > world.tick);
                i += 1;
            }
            i = 0;
            while i < world.snapshots.len() {
                metric.observe(inout out, snapshot_projection(borrow world, i), i + 1);
                i += 1;
            }
        }

        fn observe_pairs(inout out: metric.Metric, borrow world: model.World) {
            let mut i: u64 = 0;
            while i < world.entities.len() {
                let left = world.entities.get_or(i, model.empty_entity());
                let mut j = i + 1;
                while j < world.entities.len() {
                    let right = world.entities.get_or(j, model.empty_entity());
                    let dx = if left.x > right.x { left.x - right.x } else { right.x - left.x };
                    let dy = if left.y > right.y { left.y - right.y } else { right.y - left.y };
                    metric.observe(inout out, @intCast(dx + dy) +
                        left.faction * 3 + right.faction * 5 + MODE(), i + j + 1);
                    j += 1;
                }
                i += 1;
            }
        }

        fn observe_factions(inout out: metric.Metric, borrow world: model.World) {
            let mut faction: u64 = 1;
            while faction <= 4 {
                let mut agents: u64 = 0;
                let mut health: u64 = 0;
                let mut energy: u64 = 0;
                let mut inventory: u64 = 0;
                let mut i: u64 = 0;
                while i < world.entities.len() {
                    let entity = world.entities.get_or(i, model.empty_entity());
                    if entity.faction == faction && entity.alive {
                        agents += 1;
                        health += entity.health;
                        energy += entity.energy;
                        inventory += entity.inventory;
                    }
                    i += 1;
                }
                let projection = agents * (MODE() % 11 + 1) + health * 3 +
                    energy * 5 + inventory * 7 + faction;
                metric.observe(inout out, projection, faction + MODE() % 5 + 1);
                faction += 1;
            }
        }

        fn observe_regions(inout out: metric.Metric, borrow world: model.World) {
            let mut region: u64 = 0;
            while region < 16 {
                let mut tiles: u64 = 0;
                let mut walkable: u64 = 0;
                let mut resources: u64 = 0;
                let mut elevation: u64 = 0;
                let mut i: u64 = 0;
                while i < world.tiles.len() {
                    let tile = world.tiles.get_or(i, model.empty_tile());
                    if tile.region == region {
                        tiles += 1;
                        if tile.walkable { walkable += 1; }
                        resources += tile.resource;
                        elevation += tile.height;
                    }
                    i += 1;
                }
                let projection = tiles * (MODE() % 13 + 1) + walkable * 3 +
                    resources * 5 + elevation * 7 + region;
                metric.observe(inout out, projection, region % 7 + 1);
                region += 1;
            }
        }

        pub fn analyze(borrow world: model.World) -> metric.Metric {
            let mut out = metric.new(TAG());
            observe_entities(inout out, borrow world);
            observe_tiles(inout out, borrow world);
            observe_timeline(inout out, borrow world);
            observe_pairs(inout out, borrow world);
            observe_factions(inout out, borrow world);
            observe_regions(inout out, borrow world);
            metric.observe(inout out, world.resources + MODE(), 3);
            metric.observe(inout out, world.treasury + MODE(), 5);
            metric.observe(inout out, world.quests_completed + world.script_steps + MODE(), 7);
            metric.warn(inout out, world.tiles.len() != world.width * world.height);
            metric.warn(inout out, world.entities.len() == 0);
            metric.finish(inout out); out
        }

        pub fn verify(borrow world: model.World, borrow value: metric.Metric) -> bool {
            let floor = world.entities.len() + world.tiles.len() + world.events.len() +
                world.commands.len() + world.snapshots.len() + 3;
            value.tag == TAG() && value.observations >= floor && metric.valid(borrow value)
        }

        pub fn render(borrow world: model.World) -> StrBuf {
            let value = analyze(borrow world);
            let mut out = StrBuf.new();
            text.append_literal(inout out, "__LABEL__\n");
            text.line(inout out, "tag=", value.tag);
            text.line(inout out, "observations=", value.observations);
            text.line(inout out, "total=", value.total);
            text.line(inout out, "minimum=", value.minimum);
            text.line(inout out, "maximum=", value.maximum);
            text.line(inout out, "warnings=", value.warnings);
            text.line(inout out, "digest=", value.digest);
            text.bool_line(inout out, "verified=", verify(borrow world, borrow value));
            out
        }
    '''
    for index, name in enumerate(audit_names()):
        text = template.replace("__LABEL__", name).replace("__TAG__", str(20_000 + index))
        text = text.replace("__MODE__", str(index + 1))
        write(f"{name}.rue", text)


def emit_systems() -> None:
    template = r'''
        // Caldera simulation system: __LABEL__.
        const metric = @import("metric.rue");
        const model = @import("model.rue");
        const result = @import("system_result.rue");
        const std = @import("std");
        const text = @import("text.rue");
        const StrBuf = std.strbuf.StrBuf;

        pub fn SYSTEM() -> u64 { __TAG__ }
        pub fn FOCUS() -> u64 { __MODE__ }

        fn eligible_entity(borrow world: model.World, borrow entity: model.Entity) -> bool {
            if !entity.alive { return false; }
            if FOCUS() % 8 == 0 { entity.kind == model.WORKER() }
            else if FOCUS() % 8 == 1 { entity.kind == model.SCOUT() }
            else if FOCUS() % 8 == 2 { entity.kind == model.BUILDER() }
            else if FOCUS() % 8 == 3 { entity.kind == model.GUARD() }
            else if FOCUS() % 8 == 4 { entity.energy < 70 }
            else if FOCUS() % 8 == 5 { entity.inventory > 3 }
            else if FOCUS() % 8 == 6 { entity.faction == (FOCUS() % 3) + 1 }
            else { (entity.id + world.tick + FOCUS()) % 3 == 0 }
        }

        fn entity_value(borrow world: model.World, borrow entity: model.Entity) -> u64 {
            let position: u64 = @intCast(entity.x + entity.y + 8192);
            let base = if FOCUS() % 6 == 0 { entity.health + entity.energy }
                else if FOCUS() % 6 == 1 { entity.inventory * 7 + entity.credits }
                else if FOCUS() % 6 == 2 { position + entity.experience }
                else if FOCUS() % 6 == 3 { entity.kind * 31 + entity.faction * 17 }
                else if FOCUS() % 6 == 4 { entity.state * 13 + entity.updated_tick }
                else { entity.id * 19 + world.tick * 5 };
            base + FOCUS()
        }

        fn inspect_entities(inout out: result.Result, borrow world: model.World) {
            let mut i: u64 = 0;
            while i < world.entities.len() {
                let entity = world.entities.get_or(i, model.empty_entity());
                let eligible = eligible_entity(borrow world, borrow entity);
                let command = eligible && (entity.id + FOCUS() + world.tick) % 2 == 0;
                result.record(inout out, eligible, command, entity_value(borrow world, borrow entity));
                i += 1;
            }
        }

        fn inspect_tiles(inout out: result.Result, borrow world: model.World) {
            let mut i: u64 = 0;
            while i < world.tiles.len() {
                let tile = world.tiles.get_or(i, model.empty_tile());
                let eligible = tile.walkable && (tile.region + FOCUS()) % 5 == 0;
                let command = eligible && tile.resource > FOCUS() % 17;
                result.record(inout out, eligible, command,
                    tile.height + tile.resource + tile.terrain * 11 + i % 97);
                i += 1;
            }
        }

        fn inspect_timeline(inout out: result.Result, borrow world: model.World) {
            let mut i: u64 = 0;
            while i < world.events.len() {
                let event = world.events.get_or(i, model.empty_event());
                let eligible = (event.kind + FOCUS()) % 4 == 0;
                result.record(inout out, eligible, eligible && event.value > 0,
                    event.sequence + event.actor % 257 + event.value);
                i += 1;
            }
            i = 0;
            while i < world.commands.len() {
                let command = world.commands.get_or(i, model.empty_command());
                let eligible = command.accepted && (command.kind + FOCUS()) % 3 == 0;
                result.record(inout out, eligible, eligible,
                    command.tick + command.actor % 251 + command.value);
                i += 1;
            }
        }

        fn inspect_regions(inout out: result.Result, borrow world: model.World) {
            let mut region: u64 = 0;
            while region < 16 {
                let mut tiles: u64 = 0;
                let mut resources: u64 = 0;
                let mut i: u64 = 0;
                while i < world.tiles.len() {
                    let tile = world.tiles.get_or(i, model.empty_tile());
                    if tile.region == region { tiles += 1; resources += tile.resource; }
                    i += 1;
                }
                let eligible = tiles > 0 && (region + FOCUS()) % 4 == 0;
                result.record(inout out, eligible, eligible && resources > tiles,
                    resources + tiles * 13 + region);
                region += 1;
            }
        }

        pub fn evaluate(borrow world: model.World) -> result.Result {
            let mut out = result.new(SYSTEM());
            inspect_entities(inout out, borrow world);
            inspect_tiles(inout out, borrow world);
            inspect_timeline(inout out, borrow world);
            inspect_regions(inout out, borrow world);
            result.finish(inout out); out
        }

        pub fn verify(borrow world: model.World, borrow value: result.Result) -> bool {
            value.system == SYSTEM() && value.inspected >= world.entities.len() +
                world.tiles.len() + world.events.len() + world.commands.len() && value.valid
        }

        pub fn render(borrow world: model.World) -> StrBuf {
            let value = evaluate(borrow world); let mut out = StrBuf.new();
            text.append_literal(inout out, "__LABEL__\n");
            text.line(inout out, "system=", value.system);
            text.line(inout out, "inspected=", value.inspected);
            text.line(inout out, "eligible=", value.eligible);
            text.line(inout out, "commands=", value.commands);
            text.line(inout out, "energy=", value.energy);
            text.line(inout out, "digest=", value.digest);
            text.bool_line(inout out, "verified=", verify(borrow world, borrow value)); out
        }
    '''
    for index, name in enumerate(system_names()):
        text = template.replace("__LABEL__", name).replace("__TAG__", str(30_000 + index))
        text = text.replace("__MODE__", str(index + 1))
        write(f"{name}.rue", text)


def emit_behaviors() -> None:
    template = r'''
        // Caldera agent behavior: __LABEL__.
        const model = @import("model.rue");
        const result = @import("behavior_result.rue");
        const std = @import("std");
        const text = @import("text.rue");
        const StrBuf = std.strbuf.StrBuf;

        pub fn BEHAVIOR() -> u64 { __TAG__ }
        pub fn PROFILE() -> u64 { __MODE__ }

        fn role_matches(borrow entity: model.Entity) -> bool {
            if PROFILE() % 8 == 0 { entity.kind == model.WORKER() }
            else if PROFILE() % 8 == 1 { entity.kind == model.SCOUT() }
            else if PROFILE() % 8 == 2 { entity.kind == model.BUILDER() }
            else if PROFILE() % 8 == 3 { entity.kind == model.GUARD() }
            else if PROFILE() % 8 == 4 { entity.inventory > 4 }
            else if PROFILE() % 8 == 5 { entity.health < 80 }
            else if PROFILE() % 8 == 6 { entity.credits > 12 }
            else { entity.alive }
        }

        fn need(borrow world: model.World, borrow entity: model.Entity) -> u64 {
            if PROFILE() % 8 == 0 { 100 - entity.energy }
            else if PROFILE() % 8 == 1 { entity.inventory * 5 + world.resources % 37 }
            else if PROFILE() % 8 == 2 { entity.credits + world.treasury % 41 }
            else if PROFILE() % 8 == 3 { 100 - entity.health }
            else if PROFILE() % 8 == 4 { entity.experience + world.tick * 3 }
            else if PROFILE() % 8 == 5 { entity.faction * 19 + entity.kind * 23 }
            else if PROFILE() % 8 == 6 { entity.state * 29 + entity.goal % 101 }
            else { entity.id * 7 + world.quests_completed * 31 }
        }

        fn opportunity(borrow world: model.World, borrow entity: model.Entity) -> u64 {
            let index = model.tile_index(borrow world, @intCast(entity.x), @intCast(entity.y));
            let tile = world.tiles.get_or(index, model.empty_tile());
            tile.resource + tile.height + tile.terrain * 11 +
                world.resources % 97 + world.treasury % 89
        }

        fn utility(borrow world: model.World, borrow entity: model.Entity) -> u64 {
            let raw = need(borrow world, borrow entity) * (PROFILE() % 5 + 1) +
                opportunity(borrow world, borrow entity) * (PROFILE() % 7 + 1);
            raw / (PROFILE() % 3 + 1)
        }

        fn transition(borrow world: model.World, borrow entity: model.Entity, score: u64) -> bool {
            role_matches(borrow entity) && score > 20 + PROFILE() % 60 &&
                (entity.id + world.tick + PROFILE()) % 4 != 0
        }

        pub fn evaluate(borrow world: model.World) -> result.Result {
            let mut out = result.new(BEHAVIOR());
            let mut i: u64 = 0;
            while i < world.entities.len() {
                let entity = world.entities.get_or(i, model.empty_entity());
                let score = utility(borrow world, borrow entity);
                let selected = role_matches(borrow entity) &&
                    score >= (PROFILE() % 13) + entity.kind;
                result.record(inout out, selected, score,
                    selected && transition(borrow world, borrow entity, score));
                i += 1;
            }
            let mut faction: u64 = 1;
            while faction <= 3 {
                let proxy = world.resources / faction + world.treasury / (faction + 1) + PROFILE();
                result.record(inout out, proxy % 2 == 0, proxy, proxy % 5 == 0);
                faction += 1;
            }
            result.finish(inout out); out
        }

        pub fn verify(borrow world: model.World, borrow value: result.Result) -> bool {
            value.behavior == BEHAVIOR() && value.agents == world.entities.len() + 3 && value.valid
        }

        pub fn render(borrow world: model.World) -> StrBuf {
            let value = evaluate(borrow world); let mut out = StrBuf.new();
            text.append_literal(inout out, "__LABEL__\n");
            text.line(inout out, "behavior=", value.behavior);
            text.line(inout out, "agents=", value.agents);
            text.line(inout out, "selected=", value.selected);
            text.line(inout out, "utility=", value.utility);
            text.line(inout out, "transitions=", value.transitions);
            text.line(inout out, "digest=", value.digest);
            text.bool_line(inout out, "verified=", verify(borrow world, borrow value)); out
        }
    '''
    for index, name in enumerate(behavior_names()):
        text = template.replace("__LABEL__", name).replace("__TAG__", str(40_000 + index))
        text = text.replace("__MODE__", str(index + 1))
        write(f"{name}.rue", text)


def emit_rules() -> None:
    template = r'''
        // Caldera content and simulation rule: __LABEL__.
        const model = @import("model.rue");
        const result = @import("rule_result.rue");
        const std = @import("std");
        const text = @import("text.rue");
        const StrBuf = std.strbuf.StrBuf;

        pub fn RULE() -> u64 { __TAG__ }
        pub fn POLICY() -> u64 { __MODE__ }

        fn entity_match(borrow world: model.World, borrow entity: model.Entity) -> bool {
            if !entity.alive { return false; }
            if POLICY() % 6 == 0 { entity.health < 75 }
            else if POLICY() % 6 == 1 { entity.energy < 60 }
            else if POLICY() % 6 == 2 { entity.inventory > 5 }
            else if POLICY() % 6 == 3 { entity.credits > 10 }
            else if POLICY() % 6 == 4 { entity.state == POLICY() % 7 }
            else { (entity.id + world.tick + POLICY()) % 5 == 0 }
        }

        fn entity_benefit(borrow world: model.World, borrow entity: model.Entity) -> u64 {
            let pressure = (100 - entity.health) + (100 - entity.energy);
            pressure + entity.inventory * (POLICY() % 5 + 1) +
                entity.credits + world.resources % 73 + POLICY()
        }

        fn inspect_entities(inout out: result.Result, borrow world: model.World) {
            let mut i: u64 = 0;
            while i < world.entities.len() {
                let entity = world.entities.get_or(i, model.empty_entity());
                let matched = entity_match(borrow world, borrow entity);
                let benefit = entity_benefit(borrow world, borrow entity);
                result.record(inout out, matched, matched && benefit > POLICY() % 40, benefit);
                i += 1;
            }
        }

        fn inspect_tiles(inout out: result.Result, borrow world: model.World) {
            let mut i: u64 = 0;
            while i < world.tiles.len() {
                let tile = world.tiles.get_or(i, model.empty_tile());
                let matched = if POLICY() % 4 == 0 { tile.terrain == model.PLAIN() }
                    else if POLICY() % 4 == 1 { tile.terrain == model.FOREST() }
                    else if POLICY() % 4 == 2 { !tile.walkable }
                    else { tile.resource > POLICY() % 29 };
                let benefit = tile.resource + tile.height * 3 + tile.region + POLICY();
                result.record(inout out, matched, matched && benefit % 3 != 0, benefit);
                i += 1;
            }
        }

        fn inspect_events(inout out: result.Result, borrow world: model.World) {
            let mut i: u64 = 0;
            while i < world.events.len() {
                let event = world.events.get_or(i, model.empty_event());
                let matched = (event.kind + POLICY()) % 5 == 0;
                let benefit = event.value + event.sequence + event.actor % 211 + POLICY();
                result.record(inout out, matched, matched && event.tick <= world.tick, benefit);
                i += 1;
            }
        }

        fn inspect_snapshots(inout out: result.Result, borrow world: model.World) {
            let mut i: u64 = 0;
            while i < world.snapshots.len() {
                let item = world.snapshots.get_or(i, model.empty_snapshot());
                let matched = (item.tick + POLICY()) % 3 == 0;
                let benefit = item.entity_count * 7 + item.event_count * 5 +
                    item.command_count * 3 + item.checksum % 4093;
                result.record(inout out, matched, matched && item.checksum != 0, benefit);
                i += 1;
            }
        }

        pub fn evaluate(borrow world: model.World) -> result.Result {
            let mut out = result.new(RULE());
            inspect_entities(inout out, borrow world);
            inspect_tiles(inout out, borrow world);
            inspect_events(inout out, borrow world);
            inspect_snapshots(inout out, borrow world);
            result.record(inout out, world.resources > POLICY(),
                world.resources > POLICY() && world.treasury > 0,
                world.resources + world.treasury + POLICY());
            result.finish(inout out); out
        }

        pub fn verify(borrow world: model.World, borrow value: result.Result) -> bool {
            value.rule == RULE() && value.inspected == world.entities.len() +
                world.tiles.len() + world.events.len() + world.snapshots.len() + 1 && value.valid
        }

        pub fn render(borrow world: model.World) -> StrBuf {
            let value = evaluate(borrow world); let mut out = StrBuf.new();
            text.append_literal(inout out, "__LABEL__\n");
            text.line(inout out, "rule=", value.rule);
            text.line(inout out, "inspected=", value.inspected);
            text.line(inout out, "matches=", value.matches);
            text.line(inout out, "actions=", value.actions);
            text.line(inout out, "benefit=", value.benefit);
            text.line(inout out, "digest=", value.digest);
            text.bool_line(inout out, "verified=", verify(borrow world, borrow value)); out
        }
    '''
    for index, name in enumerate(rule_names()):
        text = template.replace("__LABEL__", name).replace("__TAG__", str(50_000 + index))
        text = text.replace("__MODE__", str(index + 1))
        write(f"{name}.rue", text)


def emit_reports() -> None:
    template = r'''
        // Caldera deterministic report adapter: __LABEL__.
        const checksum = @import("checksum.rue");
        const model = @import("model.rue");
        const std = @import("std");
        const text = @import("text.rue");
        const StrBuf = std.strbuf.StrBuf;

        pub fn PROFILE() -> u64 { __TAG__ }

        fn separator(inout out: StrBuf) {
            if PROFILE() % 4 == 0 { text.append_literal(inout out, ","); }
            else if PROFILE() % 4 == 1 { text.append_literal(inout out, ":"); }
            else if PROFILE() % 4 == 2 { text.append_literal(inout out, "="); }
            else { text.append_literal(inout out, " "); }
        }

        fn field(inout out: StrBuf, label: str, value: u64) {
            text.append_literal(inout out, label); separator(inout out);
            text.append_u64(inout out, value); out.push(10);
        }

        pub fn render(borrow world: model.World, suite_digest: u64) -> StrBuf {
            let mut out = StrBuf.new();
            text.append_literal(inout out, "format=__LABEL__\n");
            field(inout out, "profile", PROFILE());
            field(inout out, "tick", world.tick);
            field(inout out, "width", world.width);
            field(inout out, "height", world.height);
            field(inout out, "entities", world.entities.len());
            field(inout out, "alive", model.alive_count(borrow world));
            field(inout out, "tiles", world.tiles.len());
            field(inout out, "events", world.events.len());
            field(inout out, "commands", world.commands.len());
            field(inout out, "snapshots", world.snapshots.len());
            field(inout out, "resources", world.resources);
            field(inout out, "treasury", world.treasury);
            field(inout out, "quests", world.quests_completed);
            let mut i: u64 = 0;
            while i < world.entities.len() {
                let entity = world.entities.get_or(i, model.empty_entity());
                text.append_literal(inout out, "entity"); separator(inout out);
                text.append_u64(inout out, entity.id); separator(inout out);
                text.append_u64(inout out, entity.kind); separator(inout out);
                text.append_i64(inout out, entity.x); separator(inout out);
                text.append_i64(inout out, entity.y); separator(inout out);
                text.append_u64(inout out, entity.health); separator(inout out);
                text.append_u64(inout out, entity.state); out.push(10);
                i += 1;
            }
            field(inout out, "world_checksum", checksum.world(borrow world));
            field(inout out, "suite_digest", suite_digest);
            out
        }

        pub fn verify(borrow document: StrBuf) -> bool {
            document.len() > 48 && text.fnv1a(borrow document) != 0
        }
    '''
    for index, name in enumerate(report_names()):
        text = template.replace("__LABEL__", name).replace("__TAG__", str(60_000 + index))
        write(f"{name}.rue", text)


def emit_suites() -> None:
    audits = audit_names()
    lines = [
        "// Canonical registry for every Caldera audit pass.",
        'const metric = @import("metric.rue");',
        'const model = @import("model.rue");',
    ]
    lines.extend(f'const {name} = @import("{name}.rue");' for name in audits)
    lines.extend([
        "",
        "pub struct Summary { passes: u64, observations: u64, warnings: u64,",
        "    deterministic_mismatches: u64, digest: u64, valid: bool }",
        "",
        "fn include(inout out: Summary, borrow first: metric.Metric, borrow second: metric.Metric, verified: bool) {",
        "    out.passes += 1; out.observations += first.observations;",
        "    out.warnings += first.warnings;",
        "    if first.digest != second.digest || first.total != second.total || !verified {",
        "        out.deterministic_mismatches += 1;",
        "    }",
        "    out.digest = metric.mix(out.digest, first.digest);",
        "}",
        "",
        "pub fn run(borrow world: model.World) -> Summary {",
        "    let mut out = Summary { passes: 0, observations: 0, warnings: 0,",
        "        deterministic_mismatches: 0, digest: 14695981039346656037, valid: false };",
    ])
    for name in audits:
        lines.extend([
            f"    let {name}_a = {name}.analyze(borrow world);",
            f"    let {name}_b = {name}.analyze(borrow world);",
            f"    include(inout out, borrow {name}_a, borrow {name}_b,",
            f"        {name}.verify(borrow world, borrow {name}_a));",
        ])
    lines.extend([
        f"    out.valid = out.passes == {len(audits)} && out.warnings == 0 &&",
        "        out.deterministic_mismatches == 0; out",
        "}",
    ])
    write("audit_suite.rue", "\n".join(lines))

    systems = system_names()
    lines = [
        "// Canonical registry for every Caldera simulation system.",
        'const metric = @import("metric.rue");',
        'const model = @import("model.rue");',
        'const result = @import("system_result.rue");',
    ]
    lines.extend(f'const {name} = @import("{name}.rue");' for name in systems)
    lines.extend([
        "",
        "pub struct Summary { systems: u64, inspected: u64, eligible: u64, commands: u64,",
        "    deterministic_mismatches: u64, digest: u64, valid: bool }",
        "fn include(inout out: Summary, borrow first: result.Result, borrow second: result.Result, verified: bool) {",
        "    out.systems += 1; out.inspected += first.inspected;",
        "    out.eligible += first.eligible; out.commands += first.commands;",
        "    if first.digest != second.digest || first.energy != second.energy || !verified {",
        "        out.deterministic_mismatches += 1;",
        "    }",
        "    out.digest = metric.mix(out.digest, first.digest);",
        "}",
        "pub fn run(borrow world: model.World) -> Summary {",
        "    let mut out = Summary { systems: 0, inspected: 0, eligible: 0, commands: 0,",
        "        deterministic_mismatches: 0, digest: 14695981039346656037, valid: false };",
    ])
    for name in systems:
        lines.extend([
            f"    let {name}_a = {name}.evaluate(borrow world);",
            f"    let {name}_b = {name}.evaluate(borrow world);",
            f"    include(inout out, borrow {name}_a, borrow {name}_b,",
            f"        {name}.verify(borrow world, borrow {name}_a));",
        ])
    lines.extend([
        f"    out.valid = out.systems == {len(systems)} && out.deterministic_mismatches == 0; out",
        "}",
    ])
    write("system_suite.rue", "\n".join(lines))

    behaviors = behavior_names()
    lines = [
        "// Canonical registry for every Caldera agent behavior.",
        'const metric = @import("metric.rue");',
        'const model = @import("model.rue");',
        'const result = @import("behavior_result.rue");',
    ]
    lines.extend(f'const {name} = @import("{name}.rue");' for name in behaviors)
    lines.extend([
        "pub struct Summary { behaviors: u64, agents: u64, selected: u64, transitions: u64,",
        "    deterministic_mismatches: u64, digest: u64, valid: bool }",
        "fn include(inout out: Summary, borrow first: result.Result, borrow second: result.Result, verified: bool) {",
        "    out.behaviors += 1; out.agents += first.agents;",
        "    out.selected += first.selected; out.transitions += first.transitions;",
        "    if first.digest != second.digest || first.utility != second.utility || !verified {",
        "        out.deterministic_mismatches += 1;",
        "    }",
        "    out.digest = metric.mix(out.digest, first.digest);",
        "}",
        "pub fn run(borrow world: model.World) -> Summary {",
        "    let mut out = Summary { behaviors: 0, agents: 0, selected: 0, transitions: 0,",
        "        deterministic_mismatches: 0, digest: 14695981039346656037, valid: false };",
    ])
    for name in behaviors:
        lines.extend([
            f"    let {name}_a = {name}.evaluate(borrow world);",
            f"    let {name}_b = {name}.evaluate(borrow world);",
            f"    include(inout out, borrow {name}_a, borrow {name}_b,",
            f"        {name}.verify(borrow world, borrow {name}_a));",
        ])
    lines.extend([
        f"    out.valid = out.behaviors == {len(behaviors)} && out.deterministic_mismatches == 0; out",
        "}",
    ])
    write("behavior_suite.rue", "\n".join(lines))

    rules = rule_names()
    lines = [
        "// Canonical registry for every Caldera content rule.",
        'const metric = @import("metric.rue");',
        'const model = @import("model.rue");',
        'const result = @import("rule_result.rue");',
    ]
    lines.extend(f'const {name} = @import("{name}.rue");' for name in rules)
    lines.extend([
        "pub struct Summary { rules: u64, inspected: u64, matches: u64, actions: u64,",
        "    deterministic_mismatches: u64, digest: u64, valid: bool }",
        "fn include(inout out: Summary, borrow first: result.Result, borrow second: result.Result, verified: bool) {",
        "    out.rules += 1; out.inspected += first.inspected;",
        "    out.matches += first.matches; out.actions += first.actions;",
        "    if first.digest != second.digest || first.benefit != second.benefit || !verified {",
        "        out.deterministic_mismatches += 1;",
        "    }",
        "    out.digest = metric.mix(out.digest, first.digest);",
        "}",
        "pub fn run(borrow world: model.World) -> Summary {",
        "    let mut out = Summary { rules: 0, inspected: 0, matches: 0, actions: 0,",
        "        deterministic_mismatches: 0, digest: 14695981039346656037, valid: false };",
    ])
    for name in rules:
        lines.extend([
            f"    let {name}_a = {name}.evaluate(borrow world);",
            f"    let {name}_b = {name}.evaluate(borrow world);",
            f"    include(inout out, borrow {name}_a, borrow {name}_b,",
            f"        {name}.verify(borrow world, borrow {name}_a));",
        ])
    lines.extend([
        f"    out.valid = out.rules == {len(rules)} && out.deterministic_mismatches == 0; out",
        "}",
    ])
    write("rule_suite.rue", "\n".join(lines))

    reports = report_names()
    lines = [
        "// Canonical registry for every Caldera deterministic renderer.",
        'const metric = @import("metric.rue");',
        'const model = @import("model.rue");',
        'const std = @import("std");',
        'const text = @import("text.rue");',
        'const StrBuf = std.strbuf.StrBuf;',
    ]
    lines.extend(f'const {name} = @import("{name}.rue");' for name in reports)
    lines.extend([
        "pub struct Summary { documents: u64, bytes: u64, deterministic_mismatches: u64,",
        "    failures: u64, digest: u64, valid: bool }",
        "fn include(inout out: Summary, borrow first: StrBuf, borrow second: StrBuf, verified: bool) {",
        "    out.documents += 1; out.bytes += first.len();",
        "    let a = text.fnv1a(borrow first); let b = text.fnv1a(borrow second);",
        "    if a != b { out.deterministic_mismatches += 1; }",
        "    if !verified { out.failures += 1; }",
        "    out.digest = metric.mix(out.digest, a);",
        "}",
        "pub fn run(borrow world: model.World, suite_digest: u64) -> Summary {",
        "    let mut out = Summary { documents: 0, bytes: 0, deterministic_mismatches: 0,",
        "        failures: 0, digest: 14695981039346656037, valid: false };",
    ])
    for name in reports:
        lines.extend([
            f"    let {name}_a = {name}.render(borrow world, suite_digest);",
            f"    let {name}_b = {name}.render(borrow world, suite_digest);",
            f"    include(inout out, borrow {name}_a, borrow {name}_b, {name}.verify(borrow {name}_a));",
        ])
    lines.extend([
        f"    out.valid = out.documents == {len(reports)} && out.failures == 0 &&",
        "        out.deterministic_mismatches == 0; out",
        "}",
    ])
    write("report_suite.rue", "\n".join(lines))

CORE.update({
    "tick.rue": r'''
        // One canonical simulation tick consumed by normal execution and replay.
        const ai = @import("ai.rue");
        const economy = @import("economy.rue");
        const model = @import("model.rue");
        const physics = @import("physics.rue");
        const snapshot = @import("snapshot.rue");

        pub struct Result { tick: u64, ai_changes: u64, moved: u64,
            produced: u64, events: u64, checksum: u64, valid: bool }

        pub fn step(inout world: model.World) -> Result {
            world.tick += 1;
            let ai_changes = ai.update(inout world);
            let moved = physics.step(inout world);
            let ledger = economy.step(inout world);
            if world.tick % 3 == 0 { world.quests_completed += 1; }
            let captured = snapshot.capture(inout world);
            Result { tick: world.tick, ai_changes: ai_changes, moved: moved,
                produced: ledger.produced, events: world.events.len(),
                checksum: captured.checksum,
                valid: ledger.valid && world.valid && captured.tick == world.tick }
        }
    ''',
    "simulation.rue": r'''
        const checksum = @import("checksum.rue");
        const collision = @import("collision.rue");
        const events = @import("events.rue");
        const model = @import("model.rue");
        const tick = @import("tick.rue");

        pub struct Result { ticks: u64, entities: u64, moved: u64,
            ai_changes: u64, produced: u64, events: u64,
            collisions: u64, checksum: u64, valid: bool }

        pub fn run(inout world: model.World, count: u64) -> Result {
            let mut out = Result { ticks: 0, entities: world.entities.len(), moved: 0,
                ai_changes: 0, produced: 0, events: 0, collisions: 0,
                checksum: 0, valid: true };
            let mut i: u64 = 0;
            while i < count {
                let value = tick.step(inout world);
                out.ticks += 1; out.moved += value.moved;
                out.ai_changes += value.ai_changes;
                out.produced += value.produced;
                if !value.valid { out.valid = false; }
                i += 1;
            }
            let collision_audit = collision.audit(borrow world);
            let event_audit = events.audit(borrow world);
            out.events = world.events.len();
            out.collisions = collision_audit.tile_hits + collision_audit.entity_hits;
            out.checksum = checksum.world(borrow world);
            out.valid = out.valid && collision_audit.valid && event_audit.valid &&
                out.ticks == count && world.tick >= count;
            out
        }
    ''',
    "rollback.rue": r'''
        // Restore a baseline snapshot by deep clone, then deterministically resimulate.
        const checksum = @import("checksum.rue");
        const model = @import("model.rue");
        const simulation = @import("simulation.rue");
        const world_clone = @import("world_clone.rue");

        pub struct Result { target_tick: u64, resimulated: u64,
            checksum: u64, valid: bool }

        pub fn restore(borrow baseline: model.World, target_tick: u64) -> Result {
            let mut world = world_clone.clone(borrow baseline);
            world.rollback_count += 1;
            let count = if target_tick > world.tick { target_tick - world.tick } else { 0 };
            let run = simulation.run(inout world, count);
            Result { target_tick: target_tick, resimulated: count,
                checksum: checksum.world(borrow world),
                valid: run.valid && world.tick == target_tick && world.rollback_count > baseline.rollback_count }
        }

        pub fn agrees(borrow baseline: model.World, target_tick: u64,
            expected_checksum: u64) -> bool {
            let value = restore(borrow baseline, target_tick);
            value.valid && value.checksum == expected_checksum
        }
    ''',
    "replay.rue": r'''
        // Deterministic replay from a baseline world and a requested tick count.
        const checksum = @import("checksum.rue");
        const metric = @import("metric.rue");
        const model = @import("model.rue");
        const tick = @import("tick.rue");
        const world_clone = @import("world_clone.rue");

        pub struct Result { ticks: u64, final_checksum: u64, timeline_digest: u64,
            valid: bool }

        pub fn run(borrow baseline: model.World, count: u64) -> Result {
            let mut world = world_clone.clone(borrow baseline);
            let mut out = Result { ticks: 0, final_checksum: 0,
                timeline_digest: 14695981039346656037, valid: true };
            let mut i: u64 = 0;
            while i < count {
                let result = tick.step(inout world);
                out.ticks += 1;
                out.timeline_digest = metric.mix(out.timeline_digest, result.checksum);
                if !result.valid { out.valid = false; }
                i += 1;
            }
            out.final_checksum = checksum.world(borrow world);
            out.valid = out.valid && out.ticks == count; out
        }
    ''',
    "replay_oracle.rue": r'''
        // Oracle runs the canonical batch simulation and compares its final state.
        const checksum = @import("checksum.rue");
        const model = @import("model.rue");
        const replay = @import("replay.rue");
        const simulation = @import("simulation.rue");
        const world_clone = @import("world_clone.rue");

        pub fn agrees(borrow baseline: model.World, count: u64,
            borrow actual: replay.Result) -> bool {
            let mut world = world_clone.clone(borrow baseline);
            let expected = simulation.run(inout world, count);
            actual.valid && expected.valid && actual.ticks == expected.ticks &&
                actual.final_checksum == checksum.world(borrow world)
        }
    ''',
    "lockstep.rue": r'''
        const checksum = @import("checksum.rue");
        const model = @import("model.rue");
        const tick = @import("tick.rue");
        const world_clone = @import("world_clone.rue");

        pub struct Result { ticks: u64, mismatches: u64, checksum: u64, valid: bool }
        pub fn run(borrow baseline: model.World, count: u64) -> Result {
            let mut left = world_clone.clone(borrow baseline);
            let mut right = world_clone.clone(borrow baseline);
            let mut out = Result { ticks: 0, mismatches: 0, checksum: 0, valid: true };
            let mut i: u64 = 0;
            while i < count {
                let a = tick.step(inout left); let b = tick.step(inout right);
                if a.checksum != b.checksum { out.mismatches += 1; }
                out.ticks += 1; i += 1;
            }
            out.checksum = checksum.world(borrow left);
            out.valid = out.mismatches == 0 && out.checksum == checksum.world(borrow right);
            out
        }
    ''',
    "prediction.rue": r'''
        const model = @import("model.rue");
        const replay = @import("replay.rue");
        pub struct Prediction { horizon: u64, predicted_checksum: u64,
            confidence: u64, valid: bool }
        pub fn run(borrow baseline: model.World, horizon: u64) -> Prediction {
            let result = replay.run(borrow baseline, horizon);
            Prediction { horizon: horizon, predicted_checksum: result.final_checksum,
                confidence: if result.valid { 100 } else { 0 },
                valid: result.valid && result.ticks == horizon }
        }
    ''',
    "desync.rue": r'''
        const checksum = @import("checksum.rue");
        const metric = @import("metric.rue");
        const model = @import("model.rue");
        pub struct Audit { left: u64, right: u64, mismatch: bool,
            first_entity: u64, digest: u64, valid: bool }
        pub fn compare(borrow a: model.World, borrow b: model.World) -> Audit {
            let left = checksum.world(borrow a); let right = checksum.world(borrow b);
            let mut first = model.NONE();
            let limit = if a.entities.len() < b.entities.len() { a.entities.len() } else { b.entities.len() };
            let mut i: u64 = 0;
            while i < limit {
                let x = a.entities.get_or(i, model.empty_entity());
                let y = b.entities.get_or(i, model.empty_entity());
                if first == model.NONE() && (x.x != y.x || x.y != y.y || x.health != y.health) { first = i; }
                i += 1;
            }
            Audit { left: left, right: right, mismatch: left != right,
                first_entity: first, digest: metric.mix(left, right),
                valid: (left == right) == (first == model.NONE() &&
                    a.entities.len() == b.entities.len() && a.tick == b.tick) }
        }
    ''',
})

CORE.update({
    "world_clone.rue": r'''
        // Explicit deep clone keeps ownership transitions visible to the compiler.
        const model = @import("model.rue");

        pub fn clone(borrow source: model.World) -> model.World {
            let mut out = model.new_world(source.seed, source.width, source.height);
            out.tick = source.tick; out.resources = source.resources;
            out.treasury = source.treasury; out.quests_completed = source.quests_completed;
            out.script_steps = source.script_steps; out.rollback_count = source.rollback_count;
            out.next_entity = source.next_entity; out.next_sequence = source.next_sequence;
            out.valid = source.valid;
            let mut i: u64 = 0;
            while i < source.entities.len() { out.entities.push(source.entities.get_or(i, model.empty_entity())); i += 1; }
            i = 0;
            while i < source.tiles.len() { out.tiles.push(source.tiles.get_or(i, model.empty_tile())); i += 1; }
            i = 0;
            while i < source.events.len() { out.events.push(source.events.get_or(i, model.empty_event())); i += 1; }
            i = 0;
            while i < source.commands.len() { out.commands.push(source.commands.get_or(i, model.empty_command())); i += 1; }
            i = 0;
            while i < source.snapshots.len() { out.snapshots.push(source.snapshots.get_or(i, model.empty_snapshot())); i += 1; }
            i = 0;
            while i < source.diagnostics.len() {
                let item = source.diagnostics.get_or(i, model.Diagnostic { code: 0, subject: 0, detail: 0 });
                out.diagnostics.push(item); i += 1;
            }
            out
        }
    ''',
    "snapshot.rue": r'''
        const checksum = @import("checksum.rue");
        const metric = @import("metric.rue");
        const model = @import("model.rue");

        pub fn capture(inout world: model.World) -> model.Snapshot {
            let value = model.Snapshot { tick: world.tick,
                entity_count: world.entities.len(), event_count: world.events.len(),
                command_count: world.commands.len(), checksum: checksum.world(borrow world) };
            world.snapshots.push(model.Snapshot { tick: value.tick,
                entity_count: value.entity_count, event_count: value.event_count,
                command_count: value.command_count, checksum: value.checksum });
            value
        }

        pub struct Audit { snapshots: u64, monotonic: bool, digest: u64, valid: bool }
        pub fn audit(borrow world: model.World) -> Audit {
            let mut out = Audit { snapshots: world.snapshots.len(), monotonic: true,
                digest: 14695981039346656037, valid: false };
            let mut previous: u64 = 0;
            let mut i: u64 = 0;
            while i < world.snapshots.len() {
                let item = world.snapshots.get_or(i, model.empty_snapshot());
                if i > 0 && item.tick <= previous { out.monotonic = false; }
                previous = item.tick;
                out.digest = metric.mix(out.digest, item.checksum);
                i += 1;
            }
            out.valid = out.monotonic && out.snapshots <= world.tick + 1; out
        }
    ''',
    "delta.rue": r'''
        const checksum = @import("checksum.rue");
        const metric = @import("metric.rue");
        const model = @import("model.rue");

        pub struct Delta { entity_changes: u64, tile_changes: u64, event_changes: u64,
            before: u64, after: u64, digest: u64, valid: bool }

        pub fn compare(borrow a: model.World, borrow b: model.World) -> Delta {
            let mut entities: u64 = 0;
            let limit = if a.entities.len() < b.entities.len() { a.entities.len() } else { b.entities.len() };
            let mut i: u64 = 0;
            while i < limit {
                let left = a.entities.get_or(i, model.empty_entity());
                let right = b.entities.get_or(i, model.empty_entity());
                if left.x != right.x || left.y != right.y || left.health != right.health ||
                    left.inventory != right.inventory || left.state != right.state { entities += 1; }
                i += 1;
            }
            entities = entities + if a.entities.len() > b.entities.len() {
                a.entities.len() - b.entities.len() } else { b.entities.len() - a.entities.len() };
            let tile_changes = if a.tiles.len() == b.tiles.len() { 0 }
                else if a.tiles.len() > b.tiles.len() { a.tiles.len() - b.tiles.len() }
                else { b.tiles.len() - a.tiles.len() };
            let event_changes = if a.events.len() > b.events.len() { a.events.len() - b.events.len() }
                else { b.events.len() - a.events.len() };
            let before = checksum.world(borrow a);
            let after = checksum.world(borrow b);
            Delta { entity_changes: entities, tile_changes: tile_changes,
                event_changes: event_changes, before: before, after: after,
                digest: metric.mix(metric.mix(before, after), entities + event_changes),
                valid: (before == after) == (entities == 0 && tile_changes == 0 &&
                    event_changes == 0 && a.tick == b.tick) }
        }
    ''',
    "script_bytecode.rue": r'''
        const std = @import("std");
        pub fn PUSH() -> u64 { 1 }
        pub fn ADD() -> u64 { 2 }
        pub fn MUL() -> u64 { 3 }
        pub fn MIX() -> u64 { 4 }
        pub fn HALT() -> u64 { 5 }
        pub struct Op { kind: u64, operand: u64 }
        pub const Ops = std.arraybuf.ArrayBuf(Op);
        pub struct Program { ops: Ops, entities: u64, valid: bool }
        pub fn new() -> Program { Program { ops: Ops.new(), entities: 0, valid: true } }
        pub fn empty() -> Op { Op { kind: HALT(), operand: 0 } }
    ''',
    "script_lower.rue": r'''
        // Scenario declarations lower to a tiny stack bytecode.
        const bytecode = @import("script_bytecode.rue");
        const scenario = @import("scenario_model.rue");

        pub fn lower(borrow input: scenario.Scenario) -> bytecode.Program {
            let mut out = bytecode.new();
            out.ops.push(bytecode.Op { kind: bytecode.PUSH(), operand: input.width });
            out.ops.push(bytecode.Op { kind: bytecode.PUSH(), operand: input.height });
            out.ops.push(bytecode.Op { kind: bytecode.MUL(), operand: 0 });
            let mut i: u64 = 0;
            while i < input.entities.len() {
                let item = input.entities.get_or(i, scenario.empty_entity());
                out.ops.push(bytecode.Op { kind: bytecode.PUSH(), operand: item.kind });
                out.ops.push(bytecode.Op { kind: bytecode.PUSH(), operand: item.faction });
                out.ops.push(bytecode.Op { kind: bytecode.MUL(), operand: 0 });
                out.ops.push(bytecode.Op { kind: bytecode.ADD(), operand: 0 });
                out.ops.push(bytecode.Op { kind: bytecode.PUSH(), operand: item.x + item.y });
                out.ops.push(bytecode.Op { kind: bytecode.MIX(), operand: 0 });
                out.entities += 1; i += 1;
            }
            out.ops.push(bytecode.Op { kind: bytecode.PUSH(), operand: input.ticks });
            out.ops.push(bytecode.Op { kind: bytecode.MIX(), operand: 0 });
            out.ops.push(bytecode.Op { kind: bytecode.HALT(), operand: 0 });
            out.valid = input.valid && out.entities == input.entities.len(); out
        }
    ''',
    "script_vm.rue": r'''
        // Stack VM with explicit underflow and opcode validation.
        const bytecode = @import("script_bytecode.rue");
        const metric = @import("metric.rue");
        const model = @import("model.rue");

        pub struct Result { value: u64, steps: u64, maximum_stack: u64,
            digest: u64, valid: bool }

        pub fn execute(borrow program: bytecode.Program) -> Result {
            let mut stack = model.U64s.new();
            let mut out = Result { value: 0, steps: 0, maximum_stack: 0,
                digest: 14695981039346656037, valid: program.valid };
            let mut pc: u64 = 0;
            while pc < program.ops.len() {
                let op = program.ops.get_or(pc, bytecode.empty());
                out.steps += 1;
                out.digest = metric.mix(out.digest, op.kind * 257 + op.operand);
                if op.kind == bytecode.PUSH() { stack.push(op.operand); }
                else if op.kind == bytecode.ADD() || op.kind == bytecode.MUL() || op.kind == bytecode.MIX() {
                    if stack.len() < 2 { out.valid = false; break; }
                    let right = stack.pop_or(0); let left = stack.pop_or(0);
                    if op.kind == bytecode.ADD() { stack.push(left + right); }
                    else if op.kind == bytecode.MUL() { stack.push(left * right); }
                    else { stack.push(metric.mix(left, right)); }
                } else if op.kind == bytecode.HALT() { break; }
                else { out.valid = false; break; }
                if stack.len() > out.maximum_stack { out.maximum_stack = stack.len(); }
                pc += 1;
            }
            if stack.len() != 1 { out.valid = false; }
            out.value = stack.get_or(0, 0); out
        }
    ''',
    "script_interpreter.rue": r'''
        // Direct scenario evaluator independent of bytecode lowering and VM dispatch.
        const metric = @import("metric.rue");
        const scenario = @import("scenario_model.rue");

        pub struct Result { value: u64, entities: u64, valid: bool }

        pub fn execute(borrow input: scenario.Scenario) -> Result {
            let mut value = input.width * input.height;
            let mut i: u64 = 0;
            while i < input.entities.len() {
                let item = input.entities.get_or(i, scenario.empty_entity());
                value += item.kind * item.faction;
                value = metric.mix(value, item.x + item.y);
                i += 1;
            }
            value = metric.mix(value, input.ticks);
            Result { value: value, entities: input.entities.len(), valid: input.valid }
        }
    ''',
    "script_oracle.rue": r'''
        const bytecode = @import("script_bytecode.rue");
        const interpreter = @import("script_interpreter.rue");
        const scenario = @import("scenario_model.rue");
        const vm = @import("script_vm.rue");

        pub struct Agreement { vm_value: u64, interpreter_value: u64,
            vm_steps: u64, valid: bool }
        pub fn verify(borrow input: scenario.Scenario, borrow program: bytecode.Program) -> Agreement {
            let actual = vm.execute(borrow program);
            let expected = interpreter.execute(borrow input);
            Agreement { vm_value: actual.value, interpreter_value: expected.value,
                vm_steps: actual.steps, valid: actual.valid && expected.valid &&
                    actual.value == expected.value && program.entities == expected.entities }
        }
    ''',
    "script_disasm.rue": r'''
        const bytecode = @import("script_bytecode.rue");
        const std = @import("std");
        const text = @import("text.rue");
        const StrBuf = std.strbuf.StrBuf;
        pub fn render(borrow program: bytecode.Program) -> StrBuf {
            let mut out = StrBuf.new();
            let mut i: u64 = 0;
            while i < program.ops.len() {
                let op = program.ops.get_or(i, bytecode.empty());
                text.append_u64(inout out, i); text.append_literal(inout out, ":");
                if op.kind == bytecode.PUSH() { text.append_literal(inout out, "PUSH "); text.append_u64(inout out, op.operand); }
                else if op.kind == bytecode.ADD() { text.append_literal(inout out, "ADD"); }
                else if op.kind == bytecode.MUL() { text.append_literal(inout out, "MUL"); }
                else if op.kind == bytecode.MIX() { text.append_literal(inout out, "MIX"); }
                else { text.append_literal(inout out, "HALT"); }
                out.push(10); i += 1;
            }
            out
        }
    ''',
})

CORE.update({
    "scheduler.rue": r'''
        // Stable system scheduling order derived from live entity pressure.
        const metric = @import("metric.rue");
        const model = @import("model.rue");

        pub struct Schedule { order: model.U64s, digest: u64, valid: bool }

        pub fn priority(borrow entity: model.Entity) -> u64 {
            (if entity.health < 40 { 0 } else { 100 }) +
                entity.kind * 11 + entity.faction * 7 + (100 - entity.energy) + entity.id
        }

        pub fn build(borrow world: model.World) -> Schedule {
            let mut out = Schedule { order: model.U64s.new(),
                digest: 14695981039346656037, valid: false };
            let mut i: u64 = 0;
            while i < world.entities.len() {
                if world.entities.get_or(i, model.empty_entity()).alive { out.order.push(i); }
                i += 1;
            }
            i = 1;
            while i < out.order.len() {
                let key = out.order.get_or(i, 0);
                let key_entity = world.entities.get_or(key, model.empty_entity());
                let key_priority = priority(borrow key_entity);
                let mut j = i;
                while j > 0 {
                    let previous = out.order.get_or(j - 1, 0);
                    let previous_entity = world.entities.get_or(previous, model.empty_entity());
                    let previous_priority = priority(borrow previous_entity);
                    if previous_priority < key_priority ||
                        (previous_priority == key_priority && previous < key) { break; }
                    out.order.set(j, previous); j -= 1;
                }
                out.order.set(j, key); i += 1;
            }
            i = 0;
            let mut last: u64 = 0;
            while i < out.order.len() {
                let index = out.order.get_or(i, 0);
                let current_entity = world.entities.get_or(index, model.empty_entity());
                let current = priority(borrow current_entity);
                if i > 0 && current < last { out.valid = false; return out; }
                last = current; out.digest = metric.mix(out.digest, index);
                i += 1;
            }
            out.valid = out.order.len() == model.alive_count(borrow world);
            out
        }
    ''',
    "scheduler_oracle.rue": r'''
        // Selection-based scheduler oracle, intentionally independent of insertion sort.
        const model = @import("model.rue");
        const scheduler = @import("scheduler.rue");

        pub fn build(borrow world: model.World) -> model.U64s {
            let mut used = model.Bools.new();
            let mut i: u64 = 0;
            while i < world.entities.len() { used.push(false); i += 1; }
            let mut order = model.U64s.new();
            while order.len() < model.alive_count(borrow world) {
                let mut best = model.NONE();
                let mut best_priority = model.NONE();
                i = 0;
                while i < world.entities.len() {
                    let entity = world.entities.get_or(i, model.empty_entity());
                    if entity.alive && !used.get_or(i, true) {
                        let current = scheduler.priority(borrow entity);
                        if best == model.NONE() || current < best_priority ||
                            (current == best_priority && i < best) {
                            best = i; best_priority = current;
                        }
                    }
                    i += 1;
                }
                if best == model.NONE() { break; }
                used.set(best, true); order.push(best);
            }
            order
        }

        pub fn agrees(borrow world: model.World, borrow actual: scheduler.Schedule) -> bool {
            let expected = build(borrow world);
            if expected.len() != actual.order.len() { return false; }
            let mut i: u64 = 0;
            while i < expected.len() {
                if expected.get_or(i, model.NONE()) != actual.order.get_or(i, model.NONE()) { return false; }
                i += 1;
            }
            true
        }
    ''',
    "ai.rue": r'''
        // Closed behavior interpreter: a deliberate pre-callback design pressure probe.
        const model = @import("model.rue");

        pub fn choose(borrow world: model.World, borrow entity: model.Entity) -> u64 {
            if !entity.alive { return 0; }
            if entity.health < 35 { return 5; }
            if entity.energy < 20 { return 4; }
            if entity.kind == model.WORKER() && world.resources > 0 { return 1; }
            if entity.kind == model.SCOUT() { return 2; }
            if entity.kind == model.BUILDER() && world.treasury >= 10 { return 3; }
            if entity.kind == model.GUARD() { return 6; }
            7
        }

        pub fn update(inout world: model.World) -> u64 {
            let mut changed: u64 = 0;
            let mut i: u64 = 0;
            while i < world.entities.len() {
                let entity = world.entities.get_or(i, model.empty_entity());
                let decision = choose(borrow world, borrow entity);
                if entity.alive && decision != entity.state {
                    changed += 1;
                    world.entities.set(i, model.Entity { id: entity.id, kind: entity.kind,
                        faction: entity.faction, x: entity.x, y: entity.y,
                        previous_x: entity.previous_x, previous_y: entity.previous_y,
                        velocity_x: entity.velocity_x, velocity_y: entity.velocity_y,
                        health: entity.health, energy: entity.energy, state: decision,
                        goal: if decision == 2 { (entity.id + world.tick) % world.tiles.len() }
                            else { entity.goal },
                        inventory: entity.inventory, credits: entity.credits,
                        experience: entity.experience, born_tick: entity.born_tick,
                        updated_tick: world.tick, alive: entity.alive });
                    model.add_event(inout world, 10 + decision, entity.id, entity.goal, decision);
                }
                i += 1;
            }
            changed
        }
    ''',
    "ai_oracle.rue": r'''
        // Table-shaped independent oracle for the closed behavior interpreter.
        const ai = @import("ai.rue");
        const model = @import("model.rue");

        fn expected(borrow world: model.World, borrow entity: model.Entity) -> u64 {
            if !entity.alive { 0 }
            else if entity.health < 35 { 5 }
            else if entity.energy < 20 { 4 }
            else if entity.kind == 1 && world.resources > 0 { 1 }
            else if entity.kind == 2 { 2 }
            else if entity.kind == 3 && world.treasury >= 10 { 3 }
            else if entity.kind == 4 { 6 }
            else { 7 }
        }

        pub fn verify(borrow world: model.World) -> bool {
            let mut i: u64 = 0;
            while i < world.entities.len() {
                let entity = world.entities.get_or(i, model.empty_entity());
                if ai.choose(borrow world, borrow entity) != expected(borrow world, borrow entity) {
                    return false;
                }
                i += 1;
            }
            true
        }
    ''',
    "economy.rue": r'''
        // Resource harvesting and deterministic transfers between agents and treasury.
        const metric = @import("metric.rue");
        const model = @import("model.rue");

        pub struct Ledger { produced: u64, consumed: u64, transfers: u64,
            credits: u64, digest: u64, valid: bool }

        pub fn step(inout world: model.World) -> Ledger {
            let before = world.resources + world.treasury;
            let mut out = Ledger { produced: 0, consumed: 0, transfers: 0,
                credits: 0, digest: metric.mix(14695981039346656037, world.tick), valid: false };
            let mut i: u64 = 0;
            while i < world.entities.len() {
                let entity = world.entities.get_or(i, model.empty_entity());
                if entity.alive {
                    let produced = if entity.kind == model.WORKER() { 3 }
                        else if entity.kind == model.BUILDER() { 1 } else { 0 };
                    let consumed = if entity.kind == model.GUARD() { 2 } else { 1 };
                    out.produced += produced;
                    out.consumed += consumed;
                    let next_inventory = entity.inventory + produced;
                    let transfer = if next_inventory >= 8 { 4 } else { 0 };
                    out.transfers += transfer;
                    world.resources += produced;
                    if world.resources >= consumed + transfer {
                        world.resources = world.resources - consumed - transfer;
                        world.treasury += transfer;
                    }
                    let credit = transfer / 2;
                    out.credits += credit;
                    world.entities.set(i, model.Entity { id: entity.id, kind: entity.kind,
                        faction: entity.faction, x: entity.x, y: entity.y,
                        previous_x: entity.previous_x, previous_y: entity.previous_y,
                        velocity_x: entity.velocity_x, velocity_y: entity.velocity_y,
                        health: entity.health, energy: entity.energy, state: entity.state,
                        goal: entity.goal, inventory: next_inventory - transfer,
                        credits: entity.credits + credit, experience: entity.experience,
                        born_tick: entity.born_tick, updated_tick: world.tick, alive: entity.alive });
                    out.digest = metric.mix(out.digest, entity.id + produced * 17 + transfer * 31);
                }
                i += 1;
            }
            out.valid = world.resources + world.treasury + out.consumed + out.credits >= before;
            out
        }
    ''',
    "inventory.rue": r'''
        const metric = @import("metric.rue");
        const model = @import("model.rue");

        pub fn audit(borrow world: model.World) -> metric.Metric {
            let mut out = metric.new(1201);
            let mut i: u64 = 0;
            while i < world.entities.len() {
                let entity = world.entities.get_or(i, model.empty_entity());
                metric.observe(inout out, entity.inventory, entity.kind + 1);
                metric.warn(inout out, entity.inventory > 1000000);
                i += 1;
            }
            metric.finish(inout out); out
        }
    ''',
    "crafting.rue": r'''
        const metric = @import("metric.rue");
        const model = @import("model.rue");

        pub struct Craft { recipes: u64, craftable: u64, cost: u64, digest: u64, valid: bool }
        pub fn audit(borrow world: model.World) -> Craft {
            let mut out = Craft { recipes: 12, craftable: 0, cost: 0,
                digest: 14695981039346656037, valid: false };
            let mut recipe: u64 = 0;
            while recipe < out.recipes {
                let cost = 2 + recipe * 3;
                out.cost += cost;
                if world.resources >= cost { out.craftable += 1; }
                out.digest = metric.mix(out.digest, cost + world.seed % 97);
                recipe += 1;
            }
            out.valid = out.craftable <= out.recipes; out
        }
    ''',
    "factions.rue": r'''
        const metric = @import("metric.rue");
        const model = @import("model.rue");

        pub struct Relations { factions: u64, allied: u64, hostile: u64,
            neutral: u64, digest: u64, valid: bool }
        pub fn audit(borrow world: model.World) -> Relations {
            let mut max_faction: u64 = 0;
            let mut i: u64 = 0;
            while i < world.entities.len() {
                let value = world.entities.get_or(i, model.empty_entity()).faction;
                if value > max_faction { max_faction = value; }
                i += 1;
            }
            let mut out = Relations { factions: max_faction, allied: 0, hostile: 0,
                neutral: 0, digest: 14695981039346656037, valid: false };
            let mut a: u64 = 1;
            while a <= max_faction {
                let mut b = a;
                while b <= max_faction {
                    let relation = if a == b { 1 } else if (a + b + world.tick) % 3 == 0 { 2 } else { 3 };
                    if relation == 1 { out.allied += 1; }
                    else if relation == 2 { out.hostile += 1; }
                    else { out.neutral += 1; }
                    out.digest = metric.mix(out.digest, a * 101 + b * 17 + relation);
                    b += 1;
                }
                a += 1;
            }
            out.valid = out.allied + out.hostile + out.neutral ==
                (max_faction * (max_faction + 1)) / 2;
            out
        }
    ''',
    "quests.rue": r'''
        const metric = @import("metric.rue");
        const model = @import("model.rue");
        pub struct QuestAudit { offered: u64, active: u64, complete: u64,
            digest: u64, valid: bool }
        pub fn audit(borrow world: model.World) -> QuestAudit {
            let offered = 8 + world.entities.len();
            let active = model.alive_count(borrow world) / 2;
            let complete = world.quests_completed;
            QuestAudit { offered: offered, active: active, complete: complete,
                digest: metric.mix(metric.mix(world.seed, offered), active + complete),
                valid: active <= offered && complete <= offered + world.tick }
        }
    ''',
    "dialogue.rue": r'''
        const metric = @import("metric.rue");
        const model = @import("model.rue");
        pub struct DialogueAudit { nodes: u64, choices: u64, reachable: u64,
            digest: u64, valid: bool }
        pub fn audit(borrow world: model.World) -> DialogueAudit {
            let nodes = 24 + world.entities.len() * 2;
            let choices = nodes + world.tick;
            let reachable = if model.alive_count(borrow world) == 0 { 0 } else { nodes };
            DialogueAudit { nodes: nodes, choices: choices, reachable: reachable,
                digest: metric.mix(metric.mix(world.seed, nodes), choices),
                valid: reachable <= nodes && choices >= nodes }
        }
    ''',
})


def emit_core() -> None:
    for name, text in CORE.items():
        write(name, text)


def emit_support() -> None:
    write("demo.caldera", r'''
        # A checked-in deterministic Caldera scenario.
        WORLD 12 10;
        ENTITY WORKER 1 1 1;
        ENTITY SCOUT 1 2 2;
        ENTITY BUILDER 2 3 3;
        ENTITY GUARD 2 4 4;
        ENTITY WORKER 3 7 2;
        ENTITY SCOUT 3 8 3;
        TILE 5 4 WALL;
        TILE 5 5 WALL;
        TILE 6 5 FOREST;
        TILE 7 5 PLAIN;
        TICK 6;
        EXPECT ENTITIES 6;
    ''')
    write("README.md", r'''
        # Caldera

        Caldera is a deterministic headless simulation and game-engine laboratory
        written entirely in Rue. It is a maintained application example and the
        first Rue program whose application source exceeds 100,000 lines.

        One transitive module graph contains the fixed-point math, world model,
        physics, navigation, entity scheduling, AI, economy, scripting VM,
        snapshots, rollback, replay, lockstep networking, persistence, headless
        rendering, and observability core. Large generated families remain real
        executable code rather than padding:

        - 256 whole-world audit passes;
        - 192 simulation-system evaluators;
        - 128 agent behaviors;
        - 96 content and simulation rules;
        - 64 deterministic report adapters.

        Every family member has a distinct identity and policy, is imported by one
        canonical suite, executes twice, and contributes to determinism checks.

        Run the comprehensive cross-oracle self-test (the default invocation):

            scripts/rue exec examples/caldera/main.rue

        Run the built-in world, checked-in scenario, and invariant portfolios:

            scripts/rue exec examples/caldera/main.rue demo
            scripts/rue exec examples/caldera/main.rue run examples/caldera/demo.caldera
            scripts/rue exec examples/caldera/main.rue selftest
            scripts/rue exec examples/caldera/main.rue stress1
            scripts/rue exec examples/caldera/main.rue benchmark

        Regenerate the deterministic source families and verify the size contract:

            python3 scripts/generate-caldera.py --check

        The no-argument path executes all core oracles and all 736 generated family
        modules twice. This gives the automatic example smoke deep behavioral and
        determinism coverage while compiling the complete 100K-line graph once.
    ''')
    write("SCALING.md", r'''
        # Caldera compiler scaling snapshot

        These measurements use the same compiler revision (`bcfccc3aa94`), host, and
        regime: a release-built compiler (`--target-platforms //platforms:release`),
        the default `-O0` pipeline, a warm compiler binary, the internal linker, and
        `--benchmark-json`. Each number is the median of three runs on one Linux
        x86-64 host. They are a checked-in snapshot, not a statistically stable
        performance gate; RUE-1038 tracks retained multi-run scaling history.

        | Program | Application Rue lines | Compiler graph lines | Files | Tokens | Functions | Compile | Peak memory | Executable |
        | --- | ---: | ---: | ---: | ---: | ---: | ---: | ---: | ---: |
        | Lattice | 13,030 | 20,462 | 161 | 160,221 | 1,221 | 3.54s | 321.7MB | 1.66MB |
        | Meridian | 35,885 | 43,295 | 295 | 405,440 | 3,074 | 8.75s | 933.7MB | 4.78MB |
        | Caldera | 104,945 | 112,258 | 828 | 1,200,390 | 8,569 | 20.81s | 2.05GB | 11.46MB |

        Scaling is now essentially linear. Caldera's full source graph is 2.59 times
        Meridian's and compiles in 2.38 times the wall clock; it is 5.49 times
        Lattice's graph and compiles in 5.88 times the wall clock. Compile time per
        function is flat across the range (2.90ms, 2.85ms, and 2.43ms), and the
        deterministic per-function compiler-work counters stay within roughly 0.8 to
        1.3 times Lattice's rates across the semantic-provider, CFG, and
        query-runtime families. The distinctly nonlinear semantic scaling reported by
        the previous snapshot (80.29s for Caldera at revision `54a2eac5`) is no
        longer present.

        The remaining cost is an absolute constant factor rather than a scaling
        curve. On Caldera the query runtime performs roughly 33.5 million validation
        probe, visit, lease, and demand operations to support 213 thousand computed
        queries in a fresh single-revision process, and body-local symbol interners
        retain 12.5MB across 336 thousand entries for a program whose distinct
        identifier text is about 61KB. Peak memory remains above 2GB, and the
        compiled selftest runs in well under a second, so compilation — not Caldera
        runtime — remains the operative bottleneck.
    ''')


def main() -> int:
    parser = argparse.ArgumentParser()
    parser.add_argument("--check", action="store_true")
    args = parser.parse_args()
    OUT.mkdir(parents=True, exist_ok=True)
    emit_core()
    emit_audits()
    emit_systems()
    emit_behaviors()
    emit_rules()
    emit_reports()
    emit_suites()
    emit_support()
    digest = hashlib.sha256()
    files = sorted(OUT.glob("*.rue"))
    lines = 0
    for path in files:
        data = path.read_bytes()
        digest.update(path.name.encode())
        digest.update(data)
        lines += data.count(b"\n")
    print(f"caldera files={len(files)} lines={lines} sha256={digest.hexdigest()}")
    if args.check and lines < 100_000:
        raise SystemExit("Caldera source is below the 100,000-line contract")
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
