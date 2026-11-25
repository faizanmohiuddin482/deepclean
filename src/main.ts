import { invoke } from "@tauri-apps/api/core";

interface PathEntry {
  path: string;
  label: string;
  size: number;
  risk: "safe" | "caution";
  in_use_by: string[];
  advice: string;
}

interface CategoryResult {
  id: string;
  name: string;
  description: string;
  group: string;
  total_size: number;
  item_count: number;
  entries: PathEntry[];
  safe_to_clean: boolean;
}

interface ScanReport {
  categories: CategoryResult[];
  total_size: number;
  safe_size: number;
  caution_size: number;
  in_use_detection: boolean;
}

interface CleanResult {
  trashed: number;
  freed: number;
  errors: { path: string; error: string }[];
}

interface InvItem {
  name: string;
  detail: string;
  size: number;
  size_known: boolean;
  flags: string[];
  remove_command: string;
  path: string;
}

interface InvSection {
  id: string;
  name: string;
  subtitle: string;
  available: boolean;
  note: string;
  total_size: number;
  items: InvItem[];
}

interface Inventory {
  sections: InvSection[];
}

// ---- state -------------------------------------------------------------

let report: ScanReport | null = null;
let inventory: Inventory | null = null;
let currentView: "clean" | "inventory" = "clean";
const selected = new Set<string>();
const metaByPath = new Map<string, PathEntry>();

// amber = worth reviewing, teal = current/active, neutral otherwise
const AMBER_FLAGS = new Set(["orphan", "old", "unused"]);
const TEAL_FLAGS = new Set(["current", "leaf"]);

interface CategoryGroup {
  selectAll: HTMLInputElement;
  boxes: HTMLInputElement[];
}
const groups: CategoryGroup[] = [];

const CAT_COLORS: Record<string, string> = {
  "user-caches": "#2dd4bf",
  logs: "#818cf8",
  "xcode-derived": "#38bdf8",
  "npm-cache": "#f472b6",
  "uv-cache": "#a3e635",
  "hf-hub": "#fb923c",
  ollama: "#c084fc",
  "lm-studio": "#22d3ee",
  "ml-caches": "#60a5fa",
  "claude-code": "#f0a5c0",
  "cursor-cache": "#5eead4",
  trash: "#94a3b8",
};
const catColor = (id: string) => CAT_COLORS[id] ?? "var(--c-default)";

const reduceMotion = window.matchMedia(
  "(prefers-reduced-motion: reduce)",
).matches;

// ---- byte formatting ---------------------------------------------------

const UNITS = ["B", "KB", "MB", "GB", "TB"];

function splitBytes(bytes: number): { value: number; unit: string } {
  if (bytes <= 0) return { value: 0, unit: "B" };
  const i = Math.min(
    UNITS.length - 1,
    Math.floor(Math.log(bytes) / Math.log(1024)),
  );
  return { value: bytes / Math.pow(1024, i), unit: UNITS[i] };
}

function fmtValue(value: number, unit: string): string {
  if (unit === "B") return Math.round(value).toString();
  return (value >= 100 ? Math.round(value) : value.toFixed(1)).toString();
}

function formatBytes(bytes: number): string {
  const { value, unit } = splitBytes(bytes);
  return `${fmtValue(value, unit)} ${unit}`;
}

// ---- dom helper --------------------------------------------------------

function el<K extends keyof HTMLElementTagNameMap>(
  tag: K,
  className?: string,
  text?: string,
): HTMLElementTagNameMap[K] {
  const node = document.createElement(tag);
  if (className) node.className = className;
  if (text !== undefined) node.textContent = text;
  return node;
}

const $ = <T extends HTMLElement>(sel: string) =>
  document.querySelector<T>(sel)!;

function actionButton(
  label: string,
  title: string,
  onClick: () => void,
): HTMLButtonElement {
  const b = el("button", "entry-action", label);
  b.title = title;
  b.addEventListener("click", (e) => {
    e.preventDefault();
    e.stopPropagation();
    onClick();
  });
  return b;
}

function groupHeader(group: string): HTMLElement {
  return el("h3", "section-head", group);
}

function setStatus(message: string, kind: "info" | "error" | "success") {
  const status = $("#status");
  status.textContent = message;
  status.className = `status status-${kind}`;
}

function clearStatus() {
  $("#status").className = "status hidden";
}

function setButtonLoading(
  btn: HTMLButtonElement,
  loading: boolean,
  label: string,
) {
  btn.disabled = loading;
  btn.classList.toggle("is-loading", loading);
  btn.innerHTML = "";
  if (loading) btn.append(el("span", "btn-spinner"));
  btn.append(document.createTextNode(label));
}

// ---- hero read-out -----------------------------------------------------

function animateReadout(targetBytes: number) {
  const valueEl = $("#hero-value");
  const unitEl = $("#hero-unit");
  const { value, unit } = splitBytes(targetBytes);
  unitEl.textContent = unit;

  if (reduceMotion || value === 0) {
    valueEl.textContent = fmtValue(value, unit);
    return;
  }

  const duration = 700;
  let startTs: number | null = null;
  const step = (ts: number) => {
    if (startTs === null) startTs = ts;
    const t = Math.min(1, (ts - startTs) / duration);
    const eased = 1 - Math.pow(1 - t, 3);
    valueEl.textContent = fmtValue(value * eased, unit);
    if (t < 1) requestAnimationFrame(step);
  };
  requestAnimationFrame(step);
}

function renderHero(rep: ScanReport) {
  const cleanable = rep.safe_size + rep.caution_size;
  animateReadout(cleanable);

  $("#safe-size").textContent = formatBytes(rep.safe_size);
  $("#caution-size").textContent = formatBytes(rep.caution_size);

  const cleanCats = rep.categories.filter((c) => c.safe_to_clean);
  const trashCat = rep.categories.find((c) => !c.safe_to_clean);

  // segmented capacity bar
  const bar = $("#hero-bar");
  bar.innerHTML = "";
  const legend = $("#hero-legend");
  legend.innerHTML = "";

  const total = cleanable || 1;
  for (const cat of cleanCats) {
    const pct = (cat.total_size / total) * 100;
    const seg = el("div", "capacity-seg");
    seg.style.background = catColor(cat.id);
    seg.title = `${cat.name} — ${formatBytes(cat.total_size)}`;
    bar.append(seg);
    // grow-in after layout
    requestAnimationFrame(() => {
      seg.style.width = `${pct}%`;
    });

    const item = el("button", "legend-item");
    const dot = el("span", "legend-dot");
    dot.style.background = catColor(cat.id);
    item.append(dot, document.createTextNode(cat.name + " "));
    item.append(el("b", undefined, formatBytes(cat.total_size)));
    item.addEventListener("click", () => openAndScrollTo(cat.id));
    legend.append(item);
  }

  const trashNote = $("#trash-note");
  if (trashCat) {
    trashNote.textContent = `Plus ${formatBytes(trashCat.total_size)} sitting in your Trash — empty it from Finder to reclaim that too.`;
    trashNote.classList.remove("hidden");
  } else {
    trashNote.classList.add("hidden");
  }

  // scan pill
  const items = cleanCats.reduce((n, c) => n + c.item_count, 0);
  const inUse = cleanCats.reduce(
    (n, c) => n + c.entries.filter((e) => e.risk === "caution").length,
    0,
  );
  const pill = $("#scan-pill");
  pill.textContent =
    `${items} item${items === 1 ? "" : "s"}` +
    (inUse > 0 ? ` · ${inUse} in use` : "") +
    (rep.in_use_detection ? "" : " · process check unavailable");
  pill.classList.remove("hidden");
}

// ---- selection ---------------------------------------------------------

function selectedSize(): number {
  let total = 0;
  for (const p of selected) total += metaByPath.get(p)?.size ?? 0;
  return total;
}

function setSelected(path: string, on: boolean) {
  if (on) selected.add(path);
  else selected.delete(path);
}

function syncSelectAlls() {
  for (const g of groups) {
    g.selectAll.checked = g.boxes.length > 0 && g.boxes.every((c) => c.checked);
  }
}

function refreshSelectionUi() {
  $("#selected-size").textContent = formatBytes(selectedSize());
  const cleanBtn = $<HTMLButtonElement>("#clean-btn");
  cleanBtn.classList.remove("is-loading");
  cleanBtn.disabled = selected.size === 0;
  cleanBtn.textContent =
    selected.size === 0 ? "Clean" : `Clean ${selected.size}`;
  $("#clear-sel-btn").classList.toggle("is-hidden", selected.size === 0);
}

// ---- cards -------------------------------------------------------------

function chevron(): SVGElement {
  const svg = document.createElementNS("http://www.w3.org/2000/svg", "svg");
  svg.setAttribute("viewBox", "0 0 16 16");
  svg.setAttribute("class", "card-chevron");
  svg.setAttribute("width", "12");
  svg.setAttribute("height", "12");
  const path = document.createElementNS("http://www.w3.org/2000/svg", "path");
  path.setAttribute("d", "M6 3l5 5-5 5");
  path.setAttribute("fill", "none");
  path.setAttribute("stroke", "currentColor");
  path.setAttribute("stroke-width", "2");
  path.setAttribute("stroke-linecap", "round");
  path.setAttribute("stroke-linejoin", "round");
  svg.append(path);
  return svg;
}

function openAndScrollTo(catId: string) {
  const card = document.querySelector<HTMLElement>(`[data-cat="${catId}"]`);
  if (!card) return;
  card.classList.add("open");
  card.scrollIntoView({ behavior: reduceMotion ? "auto" : "smooth", block: "start" });
}

function renderCard(category: CategoryResult): HTMLElement {
  const card = el("section", "card");
  card.dataset.cat = category.id;

  const head = el("button", "card-head");
  const cautionCount = category.entries.filter(
    (e) => e.risk === "caution",
  ).length;

  if (category.safe_to_clean) head.append(chevron());

  const swatch = el("span", "card-swatch");
  swatch.style.background = category.safe_to_clean
    ? catColor(category.id)
    : "var(--border)";

  const info = el("div", "card-info");
  info.append(
    el("h2", "card-title", category.name),
    el("p", "card-desc", category.description),
  );

  const figures = el("div", "card-figures");
  figures.append(el("span", "card-size", formatBytes(category.total_size)));
  const sub = el("span", "card-sub");
  sub.append(document.createTextNode(`${category.item_count} items`));
  if (cautionCount > 0) {
    sub.append(document.createTextNode(" · "));
    sub.append(el("span", "caution-count", `${cautionCount} in use`));
  }
  figures.append(sub);

  head.append(swatch, info, figures);
  if (!category.safe_to_clean) {
    head.append(el("span", "badge", "report only"));
    head.style.cursor = "default";
    card.append(head);
    return card;
  }

  head.addEventListener("click", () => card.classList.toggle("open"));
  card.append(head);

  const body = el("div", "card-body");
  const inner = el("div", "card-body-inner");

  const selectAllRow = el("label", "select-all-row");
  const selectAll = el("input") as HTMLInputElement;
  selectAll.type = "checkbox";
  selectAllRow.append(selectAll, el("span", undefined, "Select all in this group"));
  selectAllRow.addEventListener("click", (e) => e.stopPropagation());
  inner.append(selectAllRow);

  const list = el("ul", "entry-list");
  const boxes: HTMLInputElement[] = [];

  for (const entry of category.entries) {
    metaByPath.set(entry.path, entry);
    const li = el("li", "entry");
    const label = el("label", "entry-label");
    const cb = el("input") as HTMLInputElement;
    cb.type = "checkbox";
    cb.dataset.path = entry.path;
    cb.addEventListener("change", () => {
      setSelected(entry.path, cb.checked);
      syncSelectAlls();
      refreshSelectionUi();
    });
    boxes.push(cb);

    const main = el("span", "entry-main");
    const name = el("span", "entry-name", entry.label);
    name.title = entry.path;
    const badgeText =
      entry.risk === "safe"
        ? "Safe"
        : entry.in_use_by.length > 0
          ? "In use"
          : "Review";
    const badge = el("span", `risk risk-${entry.risk}`, badgeText);
    badge.title = entry.advice;
    main.append(name, badge);
    if (entry.in_use_by.length > 0) {
      main.append(el("span", "entry-inuse", entry.in_use_by.join(", ")));
    }

    const actions = el("span", "entry-actions");
    actions.append(
      actionButton("Look", "Preview with Quick Look", () =>
        invoke("quicklook", { path: entry.path }).catch(() => {}),
      ),
      actionButton("Reveal", "Reveal in Finder", () =>
        invoke("reveal", { path: entry.path }).catch(() => {}),
      ),
    );

    label.append(cb, main);
    li.append(label, actions, el("span", "entry-size", formatBytes(entry.size)));
    list.append(li);
  }

  selectAll.addEventListener("change", () => {
    for (const cb of boxes) {
      cb.checked = selectAll.checked;
      setSelected(cb.dataset.path!, selectAll.checked);
    }
    refreshSelectionUi();
  });

  groups.push({ selectAll, boxes });
  inner.append(list);
  body.append(inner);
  card.append(body);
  return card;
}

// ---- render ------------------------------------------------------------

function loaderEl(title: string, note: string): HTMLElement {
  const wrap = el("div", "loader-wrap");
  wrap.append(
    el("div", "scan-orb"),
    el("p", "loader-title", title),
    el("p", "loader-note", note),
  );
  return wrap;
}

function showLoader() {
  const content = $("#content");
  content.innerHTML = "";
  content.append(
    loaderEl(
      "Reading your disk…",
      "Measuring caches, logs and build junk, and checking which items are open in running apps. Nothing is being changed.",
    ),
  );
}

function renderReport() {
  selected.clear();
  metaByPath.clear();
  groups.length = 0;
  const content = $("#content");
  content.innerHTML = "";

  if (!report || report.categories.length === 0) {
    $("#hero").classList.add("hidden");
    $("#actionbar").classList.add("hidden");
    const empty = el("div", "placeholder");
    empty.append(
      el("div", "placeholder-art"),
      el("p", "placeholder-title", "Your Mac is tidy"),
      el(
        "p",
        "placeholder-note",
        "Nothing worth reclaiming right now — macOS is keeping your caches in check. Check back after some heavy use.",
      ),
    );
    content.append(empty);
    return;
  }

  $("#hero").classList.remove("hidden");
  $("#actionbar").classList.remove("hidden");
  renderHero(report);

  let lastGroup = "";
  for (const category of report.categories) {
    if (category.group !== lastGroup) {
      content.append(groupHeader(category.group));
      lastGroup = category.group;
    }
    content.append(renderCard(category));
  }
  refreshSelectionUi();

  if (!report.in_use_detection) {
    setStatus(
      "Couldn't check running processes on this Mac, so in-use warnings may be incomplete.",
      "info",
    );
  } else {
    clearStatus();
  }
}

// ---- bulk selection ----------------------------------------------------

function selectSafeOnly() {
  selected.clear();
  for (const g of groups) {
    for (const cb of g.boxes) {
      const path = cb.dataset.path!;
      const safe = metaByPath.get(path)?.risk === "safe";
      cb.checked = safe;
      setSelected(path, safe);
    }
  }
  syncSelectAlls();
  refreshSelectionUi();
}

function clearSelection() {
  selected.clear();
  for (const g of groups) for (const cb of g.boxes) (cb.checked = false);
  syncSelectAlls();
  refreshSelectionUi();
}

// ---- confirm modal -----------------------------------------------------

function confirmClean(paths: string[]): Promise<boolean> {
  const size = formatBytes(
    paths.reduce((s, p) => s + (metaByPath.get(p)?.size ?? 0), 0),
  );
  const caution = paths
    .map((p) => metaByPath.get(p))
    .filter((e): e is PathEntry => !!e && e.risk === "caution");

  return new Promise((resolve) => {
    const overlay = el("div", "modal-overlay");
    const modal = el("div", "modal");
    modal.append(
      el("h3", undefined, "Move to Trash?"),
      el(
        "p",
        undefined,
        `${paths.length} item${paths.length === 1 ? "" : "s"} (${size}) will move to the Trash. You can restore them from Finder until you empty it.`,
      ),
    );

    if (caution.length > 0) {
      const warn = el("div", "modal-warn");
      warn.append(
        el(
          "p",
          "modal-warn-title",
          `${caution.length} item${caution.length === 1 ? " is" : "s are"} open in a running app — cleaning may interrupt:`,
        ),
      );
      const ul = el("ul", "modal-warn-list");
      for (const e of caution)
        ul.append(el("li", undefined, `${e.label} — ${e.in_use_by.join(", ")}`));
      warn.append(ul);
      modal.append(warn);
    }

    const actions = el("div", "modal-actions");
    const cancel = el("button", "btn", "Cancel");
    const confirm = el(
      "button",
      caution.length > 0 ? "btn btn-danger" : "btn btn-primary",
      caution.length > 0 ? "Clean anyway" : "Move to Trash",
    );
    actions.append(cancel, confirm);
    modal.append(actions);
    overlay.append(modal);
    document.body.append(overlay);
    confirm.focus();

    const close = (r: boolean) => {
      overlay.remove();
      document.removeEventListener("keydown", onKey);
      resolve(r);
    };
    const onKey = (e: KeyboardEvent) => {
      if (e.key === "Escape") close(false);
    };
    document.addEventListener("keydown", onKey);
    cancel.addEventListener("click", () => close(false));
    confirm.addEventListener("click", () => close(true));
    overlay.addEventListener("click", (e) => {
      if (e.target === overlay) close(false);
    });
  });
}

// ---- actions -----------------------------------------------------------

async function runScan() {
  const scanBtn = $<HTMLButtonElement>("#scan-btn");
  setButtonLoading(scanBtn, true, "Scanning…");
  clearStatus();
  showLoader();

  try {
    report = await invoke<ScanReport>("scan");
    renderReport();
  } catch (err) {
    setStatus(`Scan failed: ${String(err)}`, "error");
    $("#content").innerHTML = "";
  } finally {
    setButtonLoading(scanBtn, false, "Rescan");
  }
}

async function runClean() {
  if (selected.size === 0) return;
  const paths = [...selected];
  if (!(await confirmClean(paths))) return;

  const cleanBtn = $<HTMLButtonElement>("#clean-btn");
  setButtonLoading(cleanBtn, true, "Cleaning…");

  try {
    const result = await invoke<CleanResult>("clean", { paths });
    const freed = formatBytes(result.freed);
    if (result.errors.length === 0) {
      setStatus(
        `Done — ${result.trashed} item${result.trashed === 1 ? "" : "s"} moved to the Trash, ${freed} reclaimed.`,
        "success",
      );
    } else {
      setStatus(
        `Reclaimed ${freed}. ${result.trashed} moved, ${result.errors.length} skipped (${result.errors[0].error}).`,
        "error",
      );
    }
    await runScan();
  } catch (err) {
    setStatus(`Clean failed: ${String(err)}`, "error");
    setButtonLoading(cleanBtn, false, "Clean");
  }
}

// ---- inventory ---------------------------------------------------------

const FLAG_LABELS: Record<string, string> = {
  orphan: "Orphan",
  old: "Old",
  unused: "Unused",
  current: "Current",
  leaf: "You installed",
  dependency: "Dependency",
  cask: "Cask",
  npm: "npm",
  pipx: "pipx",
  cargo: "cargo",
};

function flagBadge(flag: string): HTMLElement {
  const tone = AMBER_FLAGS.has(flag)
    ? "tag-amber"
    : TEAL_FLAGS.has(flag)
      ? "tag-teal"
      : "tag-neutral";
  return el("span", `tag ${tone}`, FLAG_LABELS[flag] ?? flag);
}

async function copyCommand(cmd: string) {
  try {
    await invoke("copy_text", { text: cmd });
    setStatus(`Copied to clipboard:  ${cmd}`, "success");
  } catch {
    setStatus(`Run this yourself:  ${cmd}`, "info");
  }
}

function renderInvItem(item: InvItem): HTMLElement {
  const li = el("li", "entry");
  const left = el("div", "entry-label inv-item");
  const main = el("span", "entry-main");
  const name = el("span", "entry-name", item.name);
  name.title = item.path || item.name;
  main.append(name);
  for (const f of item.flags) main.append(flagBadge(f));
  left.append(main, el("span", "entry-detail", item.detail));

  const actions = el("span", "entry-actions");
  if (item.remove_command) {
    actions.append(
      actionButton("Copy", `Copy: ${item.remove_command}`, () =>
        copyCommand(item.remove_command),
      ),
    );
  }
  if (item.path) {
    actions.append(
      actionButton("Reveal", "Reveal in Finder", () =>
        invoke("reveal", { path: item.path }).catch(() => {}),
      ),
    );
  }

  const size = el(
    "span",
    "entry-size",
    item.size_known && item.size > 0 ? formatBytes(item.size) : "—",
  );
  li.append(left, actions, size);
  return li;
}

function renderInvSection(section: InvSection): HTMLElement {
  const card = el("section", "card");
  const head = el("div", "card-head");
  head.style.cursor = "default";

  const info = el("div", "card-info");
  info.append(
    el("h2", "card-title", section.name),
    el("p", "card-desc", section.available ? section.subtitle : section.note),
  );
  const figures = el("div", "card-figures");
  if (section.available && section.total_size > 0) {
    figures.append(el("span", "card-size", formatBytes(section.total_size)));
    figures.append(el("span", "card-sub", `${section.items.length} items`));
  }
  head.append(info, figures);
  card.append(head);

  if (!section.available || section.items.length === 0) return card;

  if (section.note) {
    card.append(el("p", "card-note", section.note));
  }
  const list = el("ul", "entry-list inv-list");
  for (const item of section.items) list.append(renderInvItem(item));
  card.append(list);
  return card;
}

function renderInventory() {
  const root = $("#inventory-content");
  root.innerHTML = "";
  if (!inventory) return;
  for (const section of inventory.sections) {
    root.append(renderInvSection(section));
  }
}

async function runInventory() {
  const scanBtn = $<HTMLButtonElement>("#scan-btn");
  setButtonLoading(scanBtn, true, "Scanning…");
  clearStatus();
  const root = $("#inventory-content");
  root.innerHTML = "";
  root.append(
    loaderEl(
      "Reading what's installed…",
      "Listing Homebrew, language versions, apps and global packages, and measuring sizes. Nothing is changed.",
    ),
  );
  try {
    inventory = await invoke<Inventory>("inventory");
    renderInventory();
  } catch (err) {
    setStatus(`Inventory failed: ${String(err)}`, "error");
    root.innerHTML = "";
  } finally {
    setButtonLoading(scanBtn, false, "Refresh");
  }
}

// ---- view switching ----------------------------------------------------

function switchView(view: "clean" | "inventory") {
  currentView = view;
  const clean = view === "clean";
  $("#tab-clean").classList.toggle("is-active", clean);
  $("#tab-inventory").classList.toggle("is-active", !clean);
  $("#content").classList.toggle("hidden", !clean);
  $("#inventory-content").classList.toggle("hidden", clean);
  $("#promise").classList.toggle("hidden", !clean);
  $("#hero").classList.toggle("hidden", !(clean && report));
  $("#actionbar").classList.toggle("hidden", !(clean && report));
  clearStatus();

  const scanBtn = $<HTMLButtonElement>("#scan-btn");
  if (clean) {
    scanBtn.textContent = report ? "Rescan" : "Scan";
  } else {
    scanBtn.textContent = inventory ? "Refresh" : "Scan installed";
    if (!inventory) runInventory();
  }
}

// ---- wire up -----------------------------------------------------------

window.addEventListener("DOMContentLoaded", () => {
  $("#scan-btn").addEventListener("click", () =>
    currentView === "clean" ? runScan() : runInventory(),
  );
  document.querySelector("#scan-btn-hero")?.addEventListener("click", runScan);
  $("#clean-btn").addEventListener("click", runClean);
  $("#select-safe-btn").addEventListener("click", selectSafeOnly);
  $("#clear-sel-btn").addEventListener("click", clearSelection);
  $("#tab-clean").addEventListener("click", () => switchView("clean"));
  $("#tab-inventory").addEventListener("click", () => switchView("inventory"));
});
