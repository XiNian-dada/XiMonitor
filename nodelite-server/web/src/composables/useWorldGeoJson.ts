import { ref, type Ref } from 'vue';
import {
  WORLD_GEOJSON_URL,
  type GeoJsonFeatureCollection,
} from '@/lib/map/landMask';

/**
 * Loads the world GeoJSON once and caches it at module scope so revisiting
 * the dashboard route doesn't refetch. On failure the geojson stays null and
 * the consumer falls back to the built-in land mask (matches legacy, which
 * silently keeps WORLD_LAND_POLYGONS when the fetch fails).
 */

let cached: GeoJsonFeatureCollection | null = null;
let inFlight: Promise<GeoJsonFeatureCollection> | null = null;

async function fetchWorldGeoJson(): Promise<GeoJsonFeatureCollection> {
  if (cached) return cached;
  if (!inFlight) {
    inFlight = fetch(WORLD_GEOJSON_URL, {
      cache: 'force-cache',
      headers: { accept: 'application/geo+json,application/json' },
    })
      .then((response) => {
        if (!response.ok) {
          throw new Error(`${WORLD_GEOJSON_URL} -> ${response.status}`);
        }
        return response.json() as Promise<GeoJsonFeatureCollection>;
      })
      .then((geoJson) => {
        cached = geoJson;
        return geoJson;
      })
      .catch((error: unknown) => {
        inFlight = null;
        throw error;
      });
  }
  return inFlight;
}

export function useWorldGeoJson(): {
  geojson: Ref<GeoJsonFeatureCollection | null>;
  error: Ref<Error | null>;
  load: () => Promise<void>;
} {
  const geojson = ref<GeoJsonFeatureCollection | null>(cached);
  const error = ref<Error | null>(null);

  async function load(): Promise<void> {
    try {
      geojson.value = await fetchWorldGeoJson();
    } catch (e) {
      error.value = e instanceof Error ? e : new Error(String(e));
    }
  }

  return { geojson, error, load };
}

/** Test-only: reset the module cache between specs. */
export function __resetWorldGeoJsonForTest(): void {
  cached = null;
  inFlight = null;
}
