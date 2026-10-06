interface GraphTopologyEdge {
	sourceIdx: number;
	targetIdx: number;
}

export function buildGraphTopology(nodeCount: number, edges: readonly GraphTopologyEdge[]) {
	const incidentEdges: number[][] = Array.from({ length: nodeCount }, () => []);
	const nodeDegree = new Array<number>(nodeCount).fill(0);
	const connectedSet = new Set<number>();
	for (let i = 0; i < edges.length; i++) {
		const { sourceIdx, targetIdx } = edges[i];
		connectedSet.add(sourceIdx);
		connectedSet.add(targetIdx);
		nodeDegree[sourceIdx]++;
		nodeDegree[targetIdx]++;
		incidentEdges[sourceIdx].push(i);
		// A self-edge contributes twice to degree, but only once to the neighborhood.
		if (targetIdx !== sourceIdx) incidentEdges[targetIdx].push(i);
	}
	return { incidentEdges, nodeDegree, connectedSet };
}

export function getGraphNeighborhood(
	nodeIdx: number,
	edges: readonly GraphTopologyEdge[],
	incidentEdges: readonly (readonly number[])[],
) {
	const nodeSet = new Set<number>();
	const edgeSet = new Set<number>();
	const incident = incidentEdges[nodeIdx];
	if (!incident) return { nodeSet, edgeSet };
	nodeSet.add(nodeIdx);
	for (const i of incident) {
		const edge = edges[i];
		edgeSet.add(i);
		nodeSet.add(edge.sourceIdx);
		nodeSet.add(edge.targetIdx);
	}
	return { nodeSet, edgeSet };
}
