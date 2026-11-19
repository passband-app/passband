// The theme preference, and the stylesheet's two copies of the dark tokens.
import { readFileSync } from "node:fs";
import { fileURLToPath } from "node:url";
import { describe, expect, test } from "vitest";
import { label, loadChoice, nextChoice, parseChoice } from "../src/app/theme";

const css = readFileSync(fileURLToPath(new URL("../src/ui/app.css", import.meta.url)), "utf8");

function declarations(block: string): string[] {
  return block
    .split("\n")
    .map((l) => l.trim())
    .filter((l) => l.startsWith("--") || l.startsWith("color-scheme"))
    .sort();
}

function blockAfter(marker: string): string {
  const at = css.indexOf(marker);
  if (at < 0) throw new Error(`no ${marker} in app.css`);
  const open = css.indexOf("{", at + marker.length - 1);
  const close = css.indexOf("}", open);
  return css.slice(open + 1, close);
}

describe("app.css dark tokens", () => {
  const explicit = declarations(blockAfter(':root[data-theme="dark"] {'));
  const system = declarations(blockAfter(':root:not([data-theme="light"]) {'));
  const light = declarations(blockAfter(":root {"));

  test("the explicit and the follow-the-system dark blocks are identical", () => {
    expect(explicit.length).toBeGreaterThan(40);
    expect(system).toEqual(explicit);
  });

  test("dark redefines every colour token light defines", () => {
    const names = (decls: string[]) => new Set(decls.map((d) => d.split(":")[0]));
    const darkNames = names(explicit);
    const layout = /^--(s\d+|r-|serif|sans|mono|pane-blur)/;
    const missing = [...names(light)].filter((n) => n && !layout.test(n) && !darkNames.has(n));
    expect(missing).toEqual([]);
  });

  test("dark sets color-scheme so controls and scrollbars follow", () => {
    expect(explicit).toContain("color-scheme: dark;");
  });
});

describe("theme choice", () => {
  test("anything unknown is auto", () => {
    expect(parseChoice("dark")).toBe("dark");
    expect(parseChoice("light")).toBe("light");
    expect(parseChoice("sepia")).toBe("auto");
    expect(parseChoice(null)).toBe("auto");
  });
  test("\\ cycles auto → light → dark → auto", () => {
    expect(nextChoice("auto")).toBe("light");
    expect(nextChoice("light")).toBe("dark");
    expect(nextChoice("dark")).toBe("auto");
  });
  test("blocked storage falls back to auto", () => {
    expect(
      loadChoice({
        getItem: () => {
          throw new Error("blocked");
        },
      }),
    ).toBe("auto");
    expect(loadChoice({ getItem: () => "dark" })).toBe("dark");
  });
  test("labels say what auto means", () => {
    expect(label("auto")).toMatch(/system/);
    expect(label("dark")).toBe("Theme: dark");
  });
});
