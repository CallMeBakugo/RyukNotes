export interface RepulsionNode {
	x: number;
	y: number;
	vx: number;
	vy: number;
}

/** Update velocities without moving nodes; yield between rows for cooperative layout. */
export function* applyGraphRepulsion(nodes: readonly RepulsionNode[]): Generator<void> {
	// The force already has a 600-unit cutoff. Only the same cell and its
	// eight neighbours can contain nodes inside that radius.
	const cellSize = 600;
	const columns = new Map<number, Map<number, number[]>>();
	for (let i = 0; i < nodes.length; i++) {
		const x = Math.floor(nodes[i].x / cellSize);
		const y = Math.floor(nodes[i].y / cellSize);
		let column = columns.get(x);
		if (!column) columns.set(x, column = new Map());
		let bucket = column.get(y);
		if (!bucket) column.set(y, bucket = []);
		bucket.push(i);
	}

	for (let i = 0; i < nodes.length; i++) {
		const a = nodes[i];
		const ax = a.x, ay = a.y;
		const cellX = Math.floor(ax / cellSize), cellY = Math.floor(ay / cellSize);
		for (let offsetX = -1; offsetX <= 1; offsetX++) {
			const column = columns.get(cellX + offsetX);
			if (!column) continue;
			for (let offsetY = -1; offsetY <= 1; offsetY++) {
				const bucket = column.get(cellY + offsetY);
				if (!bucket) continue;
				for (const j of bucket) {
					if (j <= i) continue;
					const b = nodes[j];
					const dx = b.x - ax, dy = b.y - ay;
					const distSq = dx * dx + dy * dy;
					if (distSq > cellSize * cellSize) continue;
					const d = distSq || 1;
					const force = 1500 / d;
					const dist = Math.sqrt(d);
					const fx = (dx / dist) * force, fy = (dy / dist) * force;
					a.vx -= fx; a.vy -= fy; b.vx += fx; b.vy += fy;
				}
			}
		}
		yield;
	}
}
