import type { GitCommit } from './ipc';

export const GRAPH_ROW_HEIGHT = 24;
export const GRAPH_LANE_WIDTH = 12;
export const graphX = (lane: number) => 12 + lane * GRAPH_LANE_WIDTH;

export interface GraphSegment {
  from: number;
  to: number;
  start: 'top' | 'node';
  end: 'node' | 'bottom';
  color: number;
}

export interface GraphRow {
  oid: string;
  lane: number;
  color: number;
  width: number;
  segments: GraphSegment[];
}

interface Lane {
  oid: string;
  color: number;
}

/** Each pending parent owns one lane, shared by every child pointing to it.
 * Keep holes until they can be reused; compacting live lanes shifts unrelated
 * branches and breaks the boundaries between adjacent SVG rows.
 */
export function layoutGitGraph(commits: readonly GitCommit[]) {
  const lanes: (Lane | null)[] = [];
  let laneCount = 1;
  const freeLane = () => {
    const free = lanes.indexOf(null);
    return free < 0 ? lanes.length : free;
  };
  const newColor = () => {
    const used = new Set(lanes.flatMap((lane) => lane ? [lane.color] : []));
    // Beyond the three distinguishable accents, geometry carries identity.
    return [0, 1, 2].find((color) => !used.has(color)) ?? 3;
  };
  const rows: GraphRow[] = commits.map((commit) => {
    let lane = lanes.findIndex((pending) => pending?.oid === commit.oid);
    const incoming = lane >= 0;
    if (!incoming) {
      lane = freeLane();
      lanes[lane] = { oid: commit.oid, color: newColor() };
    }
    const color = lanes[lane]!.color;
    const segments: GraphSegment[] = [];
    for (let index = 0; index < lanes.length; index++) {
      const pending = lanes[index];
      if (!pending || (index === lane && !incoming)) continue;
      segments.push({
        from: index, to: index, start: 'top',
        end: index === lane ? 'node' : 'bottom', color: pending.color,
      });
    }
    lanes[lane] = null;
    const parents = [...new Set(commit.parents)];
    for (const [index, oid] of parents.entries()) {
      let target = lanes.findIndex((pending) => pending?.oid === oid);
      if (target < 0) {
        target = index === 0 ? lane : freeLane();
        lanes[target] = { oid, color: index === 0 ? color : newColor() };
      }
      segments.push({ from: lane, to: target, start: 'node', end: 'bottom', color: lanes[target]!.color });
    }
    const rowLaneCount = Math.max(lane + 1, lanes.length);
    laneCount = Math.max(laneCount, rowLaneCount);
    while (lanes.length && lanes.at(-1) === null) lanes.pop();
    // Reserve room only for tracks crossing this row, rather than the widest
    // fork elsewhere in the loaded history. The title takes the remaining space.
    return { oid: commit.oid, lane, color, width: graphX(rowLaneCount - 1) + 12, segments };
  });
  return { rows, width: graphX(laneCount - 1) + 12 };
}

export function graphPath(segment: GraphSegment) {
  const fromX = graphX(segment.from);
  const toX = graphX(segment.to);
  const fromY = segment.start === 'top' ? 0 : GRAPH_ROW_HEIGHT / 2;
  const toY = segment.end === 'node' ? GRAPH_ROW_HEIGHT / 2 : GRAPH_ROW_HEIGHT;
  if (fromX === toX) return `M${fromX} ${fromY}V${toY}`;
  const middleY = (fromY + toY) / 2;
  return `M${fromX} ${fromY}C${fromX} ${middleY} ${toX} ${middleY} ${toX} ${toY}`;
}
