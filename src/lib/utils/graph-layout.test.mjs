import assert from 'node:assert/strict';
import test from 'node:test';
import { applyGraphRepulsion } from './graph-layout.ts';

const node = (x, y) => ({ x, y, vx: 0, vy: 0 });
function run(nodes) {
	for (const _ of applyGraphRepulsion(nodes)) { /* finish one physics step */ }
}

// Independent pairwise reference: an optimization must not change the force law.
function reference(nodes) {
	return nodes.map((a, index) => {
		let vx = a.vx, vy = a.vy;
		for (let other = 0; other < nodes.length; other++) {
			if (index === other) continue;
			const dx = a.x - nodes[other].x;
			const dy = a.y - nodes[other].y;
			const squared = dx * dx + dy * dy;
			if (squared > 600 * 600 || squared === 0) continue;
			const scale = 1500 / (squared * Math.sqrt(squared));
			vx += dx * scale;
			vy += dy * scale;
		}
		return { ...a, vx, vy };
	});
}

function assertForce(actual, expected) {
	for (let i = 0; i < actual.length; i++) {
		assert.equal(actual[i].x, expected[i].x, 'repulsion must not move a node mid-step');
		assert.equal(actual[i].y, expected[i].y);
		for (const axis of ['vx', 'vy']) {
			const tolerance = 1e-10 * Math.max(1, Math.abs(expected[i][axis]));
			assert.ok(Math.abs(actual[i][axis] - expected[i][axis]) <= tolerance,
				`node ${i} ${axis}: ${actual[i][axis]} != ${expected[i][axis]}`);
		}
	}
}

test('repulsion preserves nearby forces across negative coordinates and cell boundaries', () => {
	const nodes = [
		node(-601, -601), node(-599, -599), node(-1, -1), node(1, 1),
		node(599, 599), node(601, 601), node(-599, 599), node(599, -599),
		node(1800, 1800), node(10000, -10000),
	];
	nodes[0].vx = 3;
	nodes[2].vy = -2;
	const expected = reference(nodes);
	run(nodes);
	assertForce(nodes, expected);
});

test('the exact 600-unit cutoff is included, but farther nodes have no effect', () => {
	for (const offset of [[600, 0], [360, 480], [-600, 0], [0, -600]]) {
		const nodes = [node(-10, -20), node(-10 + offset[0], -20 + offset[1])];
		const expected = reference(nodes);
		run(nodes);
		assertForce(nodes, expected);
		assert.ok(Math.hypot(nodes[0].vx, nodes[0].vy) > 0);
	}
	const distant = [node(0, 0), node(600.001, 0)];
	run(distant);
	assert.deepEqual(distant, [node(0, 0), node(600.001, 0)]);
});

test('dense and coincident nodes remain finite and preserve equal-and-opposite forces', () => {
	const nodes = Array.from({ length: 120 }, (_, i) => node((i % 12) * 17 - 100, Math.floor(i / 12) * 19 - 90));
	nodes.push(node(nodes[0].x, nodes[0].y));
	const expected = reference(nodes);
	run(nodes);
	assertForce(nodes, expected);
	assert.ok(Math.abs(nodes.reduce((sum, n) => sum + n.vx, 0)) < 1e-9);
	assert.ok(Math.abs(nodes.reduce((sum, n) => sum + n.vy, 0)) < 1e-9);
});

test('moving across cells between steps updates which nodes interact', () => {
	const nodes = [node(0, 0), node(1800, 0), node(1810, 10)];
	run(nodes);
	assert.equal(nodes[0].vx, 0);
	nodes[1].x = 300;
	nodes[2].x = 310;
	const expected = reference(nodes);
	run(nodes);
	assertForce(nodes, expected);
	assert.ok(nodes[0].vx < 0);
});

test('sparse graphs do not read coordinates for every distant pair', () => {
	let coordinateReads = 0;
	const nodes = Array.from({ length: 1200 }, (_, i) => {
		const x = Math.floor(i / 3) * 1800 + (i % 3) * 20;
		return {
			get x() { coordinateReads++; return x; },
			get y() { coordinateReads++; return 0; },
			vx: 0, vy: 0,
		};
	});
	run(nodes);
	// A generous work bound, independent of wall-clock speed or exact bucket implementation.
	assert.ok(coordinateReads < nodes.length * 64,
		`distant-pair work grew quadratically: ${coordinateReads} coordinate reads`);
	assert.ok(nodes[0].vx < 0 && nodes[2].vx > 0, 'nearby triples must still repel');
});

test('empty and singleton graphs leave velocities unchanged', () => {
	run([]);
	const singleton = [{ x: 5, y: -8, vx: 2, vy: -3 }];
	run(singleton);
	assert.deepEqual(singleton, [{ x: 5, y: -8, vx: 2, vy: -3 }]);
});
