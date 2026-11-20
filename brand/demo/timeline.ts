// The film's clock, in seconds. film.ts draws against it and score.ts writes
// the soundtrack against it, so a cue moved here moves in picture and sound
// together instead of drifting a few frames apart.

export const FPS = 30;
export const DURATION = 45;

const RAW = {
  // 1. Cold open: the analyzer wide open, the site's first beat.
  lede: 0.4,
  human: 1.2,
  openOut: 3.0,

  // 2. The noise has names: a wall of subject lines, the unread count racing.
  wallIn: 3.2,
  countFrom: 3.4,
  countTo: 5.6,
  wallCaption: 4.4,
  glint: 6.3, // the three that matter catch the light, unmarked until now
  wallOut: 7.0,

  // 3. The squelch closes: the filter narrows and three carriers stand clear.
  squelchFrom: 7.3,
  squelchTo: 9.9,
  markers: 10.2,
  threeNeed: 10.5,
  scopeOut: 13.0,

  // 4. The sitrep: the app itself, rising out of the scene.
  appIn: 13.2,
  appTitle: 13.6,
  zoomFye: 15.4,

  // 5. Done in a keystroke.
  key1: 18.4,
  key2: 20.2,

  // 6. A rule in plain English. Typing runs one character per 32nd from
  // typeFrom; typeTo and ruleSave follow from the text's length.
  ruleKey: 22.6,
  ruleOpen: 22.9,
  typeFrom: 23.4,
  appOut: 28.2,

  // 7. The trust row.
  factsIn: 28.8,
  factsOut: 34.6,

  // 8. The close.
  endIn: 35.2,
  endCta: 37.2,
  fadeOut: 43.6,
};

// ---------------------------------------------------------------- the grid
// The techno runs at 138 bpm with the app's entrance on a downbeat. The
// on-screen actions are snapped to it, because their sounds are the kit's
// own hats and claps: a keystroke off the grid would be a drummer missing.
export const BPM = 138;
export const BEAT = 60 / BPM;
export const BAR = 4 * BEAT;
/// Snap t to the nearest 1/div of a beat on the film's grid.
export const onGrid = (t: number, div: number) =>
  RAW.appIn + Math.round(((t - RAW.appIn) * div) / BEAT) * (BEAT / div);

export const RULE_TEXT = "Only show me discounts 25% off or more.";
const typeFrom = onGrid(RAW.typeFrom, 8);
const typeTo = typeFrom + (RULE_TEXT.length - 1) * (BEAT / 8);

export const T = {
  ...RAW,
  key1: onGrid(RAW.key1, 2),
  key2: onGrid(RAW.key2, 2),
  ruleKey: onGrid(RAW.ruleKey, 2),
  typeFrom,
  typeTo,
  // The save lands on the first beat at least an eighth after the last key.
  ruleSave: RAW.appIn + Math.ceil((typeTo + BEAT / 2 - RAW.appIn) / BEAT) * BEAT,
} as const;

// The doomscroll's flicks, relative to T.wallIn: [start, duration, pixels].
// The wall scrolls on them and the score whooshes on them.
export const FLICKS: Array<[start: number, dur: number, dist: number]> = [
  [0.15, 0.8, 256], [1.05, 0.85, 320], [1.95, 0.8, 256],
];

// The passband's centre and the drone that fills it: three sines in fifths,
// which are the analyzer's three carriers. score.ts plays them; film.ts maps
// its frequency axis so they land where the markers are.
export const PASS_HZ = 165;
export const DRONE = [
  { f: 110, a: 0.8 },
  { f: 165, a: 1.0 },
  { f: 247.5, a: 0.62 },
];


/// When each rule character lands: straight 32nds, one per character, so the
/// typing plays as a quick, steady hat run on the grid.
export function typeTimes(): number[] {
  return [...RULE_TEXT].map((_, i) => T.typeFrom + i * (BEAT / 8));
}
