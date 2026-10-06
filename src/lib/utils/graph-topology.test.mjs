import assert from 'node:assert/strict';
import test from 'node:test';
import { buildGraphTopology, getGraphNeighborhood } from './graph-topology.ts';

const edge = (sourceIdx, targetIdx) => ({ sourceIdx, targetIdx });

function scannedNeighborhood(nodeIdx, edges) {
  const nodeSet = new Set();
  const edgeSet = new Set();
  if (nodeIdx < 0) return { nodeSet, edgeSet };
  nodeSet.add(nodeIdx);
  edges.forEach((e, i) => {
    if (e.sourceIdx === nodeIdx || e.targetIdx === nodeIdx) {
      edgeSet.add(i);
      nodeSet.add(e.sourceIdx);
      nodeSet.add(e.targetIdx);
    }
  });
  return { nodeSet, edgeSet };
}

test('topology retains duplicate edges and counts self-edges twice in degree but once in adjacency', () => {
  const edges = [edge(0, 1), edge(0, 1), edge(1, 0), edge(0, 0), edge(2, 2)];
  const topology = buildGraphTopology(4, edges);
  assert.deepEqual(topology.incidentEdges, [[0, 1, 2, 3], [0, 1, 2], [4], []]);
  assert.deepEqual(topology.nodeDegree, [5, 3, 2, 0]);
  assert.deepEqual(topology.connectedSet, new Set([0, 1, 2]));
  assert.deepEqual(getGraphNeighborhood(0, edges, topology.incidentEdges), {
    nodeSet: new Set([0, 1]),
    edgeSet: new Set([0, 1, 2, 3]),
  });
});

test('changing active nodes produces only direct neighbors without changing prior sets', () => {
  const edges = [edge(0, 1), edge(2, 1), edge(2, 3), edge(3, 4)];
  const { incidentEdges } = buildGraphTopology(6, edges);
  const first = getGraphNeighborhood(0, edges, incidentEdges);
  const second = getGraphNeighborhood(2, edges, incidentEdges);
  assert.deepEqual(first, { nodeSet: new Set([0, 1]), edgeSet: new Set([0]) });
  assert.deepEqual(second, { nodeSet: new Set([2, 1, 3]), edgeSet: new Set([1, 2]) });
  assert.deepEqual(getGraphNeighborhood(5, edges, incidentEdges), {
    nodeSet: new Set([5]), edgeSet: new Set(),
  });
  assert.deepEqual(getGraphNeighborhood(-1, edges, incidentEdges), {
    nodeSet: new Set(), edgeSet: new Set(),
  });
});

test('rebuilding topology uses the replacement edges for the same active node', () => {
  const beforeEdges = [edge(0, 1)];
  const afterEdges = [edge(2, 0), edge(0, 0)];
  const before = buildGraphTopology(3, beforeEdges);
  const after = buildGraphTopology(3, afterEdges);
  assert.deepEqual(getGraphNeighborhood(0, beforeEdges, before.incidentEdges), {
    nodeSet: new Set([0, 1]), edgeSet: new Set([0]),
  });
  assert.deepEqual(getGraphNeighborhood(0, afterEdges, after.incidentEdges), {
    nodeSet: new Set([0, 2]), edgeSet: new Set([0, 1]),
  });
});

test('indexed neighborhoods match full scans for directed, duplicate and self edges', () => {
  const edges = Array.from({ length: 150 }, (_, i) => edge(i % 13, (i * 7 + 3) % 13));
  edges.push(edge(0, 0), edge(0, 1), edge(0, 1));
  const { incidentEdges } = buildGraphTopology(14, edges);
  for (let nodeIdx = -1; nodeIdx < 14; nodeIdx++) {
    assert.deepEqual(getGraphNeighborhood(nodeIdx, edges, incidentEdges), scannedNeighborhood(nodeIdx, edges));
  }
});

test('construction reads each endpoint once and neighborhood work visits only incident edges', () => {
  let endpointReads = 0;
  const pairs = [[0, 1], [0, 0], [2, 0], ...Array.from({ length: 1000 }, () => [2, 3])];
  const edges = pairs.map(([source, target]) => ({
    get sourceIdx() { endpointReads++; return source; },
    get targetIdx() { endpointReads++; return target; },
  }));
  const { incidentEdges } = buildGraphTopology(5, edges);
  assert.equal(endpointReads, edges.length * 2);
  endpointReads = 0;
  assert.deepEqual(getGraphNeighborhood(0, edges, incidentEdges), {
    nodeSet: new Set([0, 1, 2]), edgeSet: new Set([0, 1, 2]),
  });
  assert.equal(endpointReads, 6);
  endpointReads = 0;
  getGraphNeighborhood(4, edges, incidentEdges);
  getGraphNeighborhood(-1, edges, incidentEdges);
  assert.equal(endpointReads, 0);
});

test('an empty graph has no degree, adjacency or neighborhood entries', () => {
  const topology = buildGraphTopology(0, []);
  assert.deepEqual(topology, { incidentEdges: [], nodeDegree: [], connectedSet: new Set() });
  assert.deepEqual(getGraphNeighborhood(-1, [], topology.incidentEdges), {
    nodeSet: new Set(), edgeSet: new Set(),
  });
});
