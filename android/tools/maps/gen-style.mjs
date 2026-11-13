// Generates the Pingers map style from @protomaps/basemaps (light flavour), recoloured to the
// app's map palette, with local glyphs and sprites. Usage: node gen-style.mjs <basemaps esm index> <out.json>
const [, , modulePath, out] = process.argv;
const { layers, namedFlavor } = await import(modulePath);
import { writeFileSync } from "node:fs";

const base = namedFlavor("light");
// Pingers' placeholder palette (ui/theme Pc.MapLand, MapWater, MapPark) so real maps read like the canvas.
const flavor = {
  ...base,
  background: "#EEF0EA",
  earth: "#EEF0EA",
  water: "#C9DDF0",
  park_a: "#D7E8CF",
  park_b: "#D7E8CF",
  wood_a: "#D7E8CF",
  wood_b: "#D7E8CF",
  scrub_a: "#DDEBD4",
  scrub_b: "#DDEBD4",
};
const style = {
  version: 8,
  name: "Pingers",
  glyphs: "asset://maps/glyphs/{fontstack}/{range}.pbf",
  sprite: "asset://maps/sprites/light",
  sources: {
    protomaps: {
      type: "vector",
      url: "{{PACK_URL}}",
      attribution: "© OpenStreetMap contributors",
    },
  },
  layers: layers("protomaps", flavor, { lang: "en" }),
};
// Font stacks without spaces, so asset paths need no escaping.
const fontNames = new Set();
const rename = (font) => {
  const renamed = font.replace(/ /g, "");
  fontNames.add(renamed);
  return renamed;
};
const walk = (value) => {
  if (Array.isArray(value)) return value.map(walk);
  if (value && typeof value === "object") {
    const result = {};
    for (const [key, inner] of Object.entries(value)) {
      result[key] = key === "text-font" ? renameFonts(inner) : walk(inner);
    }
    return result;
  }
  return value;
};
const renameFonts = (value) => {
  if (Array.isArray(value)) {
    return value.map((item) => (typeof item === "string" && item.startsWith("Noto") ? rename(item) : renameFonts(item)));
  }
  return value;
};
style.layers = walk(style.layers);
writeFileSync(out, JSON.stringify(style));
console.log(`${style.layers.length} layers; fonts: ${[...fontNames].join(", ")}`);
