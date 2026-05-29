/**
 * Land-mask rendering, ported from the legacy paintWorldDotMap pipeline in
 * assets/index.html. The mask is drawn (GeoJSON or built-in fallback) onto an
 * offscreen canvas, then sampled on a grid to stamp a dotted world map.
 *
 * Drawing fns take an injected context so they're unit-testable with a mock;
 * sampleDots (which pixels become dots) is pure and tested directly. Only
 * paintWorldDotMap touches the real DOM/canvas — guarded for null contexts.
 */

import {
  MAP_DOT_GAP,
  MAP_DOT_SIZE,
  MAP_HEIGHT,
  MAP_MIN_LAT,
  MAP_WIDTH,
  mapProject,
} from './projection';

export const WORLD_GEOJSON_URL =
  'https://raw.githubusercontent.com/datasets/geo-boundaries-world-110m/master/countries.geojson';

type Coord = [number, number];
type Ring = Coord[];
type Polygon = Ring[];

export interface GeoJsonFeatureCollection {
  features?: Array<{
    geometry?: { type?: string; coordinates?: unknown };
  }>;
}

/** Coarse built-in land outline; used when the GeoJSON fetch fails. */
export const WORLD_LAND_POLYGONS: Ring[] = [
  [[-168,72],[-150,69],[-137,60],[-127,55],[-124,49],[-129,43],[-124,33],[-116,29],[-108,23],[-97,18],[-90,19],[-82,25],[-80,31],[-75,38],[-67,45],[-55,48],[-51,55],[-64,64],[-82,70],[-110,73],[-138,73]],
  [[-169,63],[-154,59],[-143,59],[-139,55],[-151,53],[-164,56]],
  [[-92,18],[-85,21],[-78,19],[-77,12],[-83,8],[-90,12],[-99,17],[-106,23],[-112,29],[-106,26],[-98,22]],
  [[-84,22],[-77,23],[-71,20],[-75,17],[-83,18]],
  [[-78,27],[-73,25],[-70,21],[-75,20],[-80,23]],
  [[-82,12],[-74,11],[-66,5],[-58,-5],[-48,-15],[-38,-22],[-40,-34],[-51,-45],[-64,-55],[-73,-50],[-76,-35],[-80,-20],[-80,-5]],
  [[-53,82],[-34,81],[-20,75],[-18,65],[-33,61],[-48,62],[-63,70]],
  [[-11,71],[6,70],[22,66],[36,61],[55,60],[74,68],[96,72],[119,70],[143,62],[166,58],[179,51],[168,43],[145,40],[125,34],[116,25],[104,21],[96,14],[88,8],[77,9],[70,18],[63,25],[52,29],[42,36],[30,42],[18,44],[9,48],[0,52],[-8,50],[-20,58]],
  [[5,58],[16,60],[28,67],[31,71],[19,72],[10,66]],
  [[-10,36],[2,37],[16,34],[29,31],[39,20],[50,12],[47,-7],[43,-26],[33,-34],[18,-35],[8,-27],[-4,-15],[-13,0],[-17,16]],
  [[33,31],[42,31],[55,26],[58,17],[51,12],[42,16],[36,23]],
  [[68,24],[80,27],[91,24],[93,15],[87,7],[77,7],[70,15]],
  [[94,22],[104,22],[112,18],[122,8],[124,-2],[117,-8],[106,-6],[98,2]],
  [[95,6],[106,7],[112,0],[105,-6],[96,-4]],
  [[109,6],[116,7],[119,0],[113,-4],[107,-1]],
  [[118,7],[126,6],[125,-4],[117,-5]],
  [[128,0],[142,-3],[142,-9],[130,-9]],
  [[120,16],[124,13],[123,7],[119,8]],
  [[111,-11],[128,-14],[145,-16],[154,-25],[150,-37],[133,-43],[116,-36],[110,-22]],
  [[141,-4],[153,-4],[153,-10],[141,-10]],
  [[43,-12],[50,-14],[51,-22],[47,-26],[43,-21]],
  [[138,46],[146,43],[145,36],[140,31],[134,34],[132,39]],
  [[127,38],[130,39],[130,34],[126,34]],
  [[120,25],[123,24],[123,21],[120,21],[119,23]],
  [[166,-34],[179,-37],[178,-44],[170,-46],[166,-42]],
  [[144,-40],[149,-42],[148,-44],[143,-43]],
  [[-8,58],[2,57],[1,50],[-6,50]],
  [[-10,55],[-5,54],[-6,51],[-10,51]],
  [[12,56],[25,60],[31,66],[20,69],[11,63]],
  [[79,8],[82,8],[82,5],[79,5]],
  [[34,35],[36,35],[36,32],[34,32]],
];

export function polygonMaxLat(polygon: Polygon): number {
  let maxLat = -90;
  for (const ring of polygon || []) {
    for (const coord of ring || []) {
      const lat = Number(coord[1]);
      if (Number.isFinite(lat)) maxLat = Math.max(maxLat, lat);
    }
  }
  return maxLat;
}

export function drawProjectedRing(ctx: CanvasRenderingContext2D, ring: Ring): void {
  let started = false;
  let lastLon: number | null = null;
  for (const coord of ring) {
    const lon = Number(coord[0]);
    const lat = Number(coord[1]);
    if (!Number.isFinite(lon) || !Number.isFinite(lat)) continue;
    const point = mapProject(lon, lat);
    if (!started || (lastLon != null && Math.abs(lon - lastLon) > 180)) {
      ctx.moveTo(point.x, point.y);
      started = true;
    } else {
      ctx.lineTo(point.x, point.y);
    }
    lastLon = lon;
  }
}

export function drawGeoJsonMask(
  ctx: CanvasRenderingContext2D,
  geoJson: GeoJsonFeatureCollection,
): void {
  ctx.beginPath();
  for (const feature of geoJson.features || []) {
    const geometry = feature.geometry || {};
    if (geometry.type === 'Polygon') {
      const coordinates = geometry.coordinates as Polygon;
      if (polygonMaxLat(coordinates) < MAP_MIN_LAT) continue;
      for (const ring of coordinates || []) drawProjectedRing(ctx, ring);
    } else if (geometry.type === 'MultiPolygon') {
      const coordinates = geometry.coordinates as Polygon[];
      for (const polygon of coordinates || []) {
        if (polygonMaxLat(polygon) < MAP_MIN_LAT) continue;
        for (const ring of polygon || []) drawProjectedRing(ctx, ring);
      }
    }
  }
  ctx.fill('evenodd');
}

export function drawFallbackMask(ctx: CanvasRenderingContext2D): void {
  ctx.beginPath();
  for (const polygon of WORLD_LAND_POLYGONS) drawProjectedRing(ctx, polygon);
  ctx.fill('evenodd');
}

/**
 * Walk the mask's pixels on a grid and return the centres where land was
 * painted. A pixel counts as land unless it's fully transparent black
 * (r === 0 && alpha === 0), matching the legacy sampling test. Pure.
 */
export function sampleDots(
  pixels: Uint8ClampedArray,
  width = MAP_WIDTH,
  height = MAP_HEIGHT,
  gap = MAP_DOT_GAP,
): Array<{ x: number; y: number }> {
  const dots: Array<{ x: number; y: number }> = [];
  for (let y = 0; y < height; y += gap) {
    for (let x = 0; x < width; x += gap) {
      const index = ((y | 0) * width + (x | 0)) * 4;
      if (pixels[index] === 0 && pixels[index + 3] === 0) continue;
      dots.push({ x, y });
    }
  }
  return dots;
}

export type MaskPainter = (ctx: CanvasRenderingContext2D) => void;

/**
 * Safe getContext: returns null instead of throwing when the environment has
 * no canvas implementation. jsdom *throws* "Not implemented" rather than
 * returning null, so a plain `getContext('2d')` would surface in unit tests.
 */
function get2dContext(
  canvas: HTMLCanvasElement,
  options?: CanvasRenderingContext2DSettings,
): CanvasRenderingContext2D | null {
  try {
    return canvas.getContext('2d', options);
  } catch {
    return null;
  }
}

/**
 * Orchestration glue (touches the DOM). Renders the mask to an offscreen
 * canvas, samples it, and stamps dots onto the target canvas. No-ops if a 2D
 * context is unavailable (e.g. jsdom) so callers don't need to guard.
 */
export function paintWorldDotMap(
  target: HTMLCanvasElement,
  maskPainter: MaskPainter,
  dotColor: string,
): void {
  const ctx = get2dContext(target);
  if (!ctx) return;
  target.width = MAP_WIDTH;
  target.height = MAP_HEIGHT;

  const maskCanvas = document.createElement('canvas');
  maskCanvas.width = MAP_WIDTH;
  maskCanvas.height = MAP_HEIGHT;
  const mask = get2dContext(maskCanvas, { willReadFrequently: true });
  if (!mask) return;
  mask.clearRect(0, 0, MAP_WIDTH, MAP_HEIGHT);
  mask.fillStyle = '#fff';
  maskPainter(mask);

  const pixels = mask.getImageData(0, 0, MAP_WIDTH, MAP_HEIGHT).data;
  ctx.clearRect(0, 0, MAP_WIDTH, MAP_HEIGHT);
  ctx.fillStyle = dotColor;
  ctx.beginPath();
  for (const dot of sampleDots(pixels)) {
    ctx.moveTo(dot.x + MAP_DOT_SIZE, dot.y);
    ctx.arc(dot.x, dot.y, MAP_DOT_SIZE, 0, Math.PI * 2);
  }
  ctx.fill();
}
