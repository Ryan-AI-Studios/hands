const TEXT_CAP = 80;
const MAIN_TEXT_CAP = 2000;
const CARD_TITLE_CAP = 80;
const CARD_PRICE_CAP = 24;
const CARD_HREF_CAP = 200;
const CARD_CAP = 8;
const CARD_MILES_CAP = 16;
const CARD_DEALER_CAP = 48;
const CARD_DISTANCE_CAP = 40;
const CARD_OF_CAP = 12;
const RESULT_COUNT_CAP = 24;
const LOCAL_MATCHES_CAP = 24;
const EMPTY_STATE_CAP = 120;
const ZIP_CAP = 10;
const RADIUS_CAP = 16;
const PRICE_RE = /\$|€|£|\d[\d,]*\.\d{2}/;

const DEFAULT_SELECTOR = [
  "a[href]",
  "button",
  "input:not([type=hidden])",
  "textarea",
  "select",
  '[role="button"]',
  '[role="link"]',
  '[role="tab"]',
  '[role="menuitem"]',
  '[role="checkbox"]',
  '[role="radio"]',
  '[role="textbox"]',
  '[role="combobox"]',
  '[contenteditable=""]',
  '[contenteditable="true"]',
].join(",");

const DOM_SELECTOR =
  DEFAULT_SELECTOR +
  ",[role],summary,label,[tabindex]:not([tabindex=\"-1\"])";

chrome.runtime.onMessage.addListener((msg, _sender, sendResponse) => {
  const op = msg && msg.op;
  if (op !== "snapshot" && op !== "resolve") {
    return false;
  }
  try {
    if (op === "snapshot") {
      sendResponse(buildSnapshot(msg.detail));
    } else {
      sendResponse(buildResolve(msg.id, msg.detail));
    }
  } catch (_err) {
    sendResponse({ error: "walk-failed" });
  }
  return false;
});

function buildSnapshot(detail) {
  const metrics = windowMetrics();
  const nodes = walk(detail);
  const elements = [];
  for (let i = 0; i < nodes.length; i += 1) {
    const payload = nodePayload(nodes[i], i);
    if (payload) {
      elements.push(payload);
    }
  }
  const snap = {
    url: location.href || "",
    title: document.title || "",
    main_text: mainText(),
    elements: elements,
    cards: collectCards(),
    metrics: metrics,
  };
  const listing = collectListingMeta();
  if (listing.result_count) snap.result_count = listing.result_count;
  if (listing.local_matches) snap.local_matches = listing.local_matches;
  if (listing.empty_state) snap.empty_state = listing.empty_state;
  if (listing.zip) snap.zip = listing.zip;
  if (listing.radius) snap.radius = listing.radius;
  return snap;
}

function buildResolve(id, detail) {
  const index = parseChrId(id);
  const metrics = windowMetrics();
  if (index == null) {
    return { error: "not-found", metrics: metrics };
  }
  const nodes = walk(detail);
  if (index >= nodes.length) {
    return { error: "not-found", metrics: metrics };
  }
  const payload = nodePayload(nodes[index], index);
  if (!payload) {
    return { error: "not-found", metrics: metrics };
  }
  return {
    id: payload.id,
    role: payload.role,
    text: payload.text,
    rectCss: payload.rectCss,
    href: payload.href,
    metrics: metrics,
  };
}

function parseChrId(id) {
  if (typeof id === "number" && Number.isInteger(id) && id >= 0) {
    return id;
  }
  if (typeof id !== "string") {
    return null;
  }
  const m = /^chr:(0|[1-9]\d*)$/.exec(id);
  if (!m) {
    return null;
  }
  const n = Number(m[1]);
  if (!Number.isFinite(n) || n > 4294967295) {
    return null;
  }
  return n;
}

function walk(detail) {
  const selector = detail === "dom" ? DOM_SELECTOR : DEFAULT_SELECTOR;
  const list = document.querySelectorAll(selector);
  const out = [];
  for (let i = 0; i < list.length; i += 1) {
    const el = list[i];
    if (includeNode(el)) {
      out.push(el);
    }
  }
  return out;
}

function includeNode(el) {
  if (!el || el.nodeType !== 1) {
    return false;
  }
  if (el.disabled || el.hasAttribute("disabled")) {
    return false;
  }
  if (el.hidden || el.hasAttribute("hidden")) {
    return false;
  }
  if (el.getAttribute("aria-hidden") === "true") {
    return false;
  }
  const rect = el.getBoundingClientRect();
  if (!rect || rect.width <= 0 || rect.height <= 0) {
    return false;
  }
  const style = window.getComputedStyle(el);
  if (!style) {
    return false;
  }
  if (style.display === "none" || style.visibility === "hidden") {
    return false;
  }
  return true;
}

function nodePayload(el, index) {
  const rect = cssRect(el);
  if (!rect) {
    return null;
  }
  const password = isPassword(el);
  const href = hrefOf(el);
  const payload = {
    id: "chr:" + String(index),
    role: mapRole(el),
    text: password ? null : nodeText(el),
    rectCss: rect,
  };
  if (href) {
    payload.href = href;
  }
  return payload;
}

function cssRect(el) {
  const r = el.getBoundingClientRect();
  if (!r || r.width <= 0 || r.height <= 0) {
    return null;
  }
  return {
    left: r.left,
    top: r.top,
    width: r.width,
    height: r.height,
  };
}

function isPassword(el) {
  return el.tagName === "INPUT" && String(el.type || "").toLowerCase() === "password";
}

function nodeText(el) {
  const aria = attr(el, "aria-label");
  if (aria) {
    return cap(aria, TEXT_CAP);
  }
  const placeholder = attr(el, "placeholder");
  if (placeholder) {
    return cap(placeholder, TEXT_CAP);
  }
  const alt = attr(el, "alt");
  if (alt) {
    return cap(alt, TEXT_CAP);
  }
  const raw = (el.innerText || "").replace(/\s+/g, " ").trim();
  return cap(raw, TEXT_CAP);
}

function attr(el, name) {
  const v = el.getAttribute(name);
  if (!v) {
    return "";
  }
  return v.replace(/\s+/g, " ").trim();
}

function hrefOf(el) {
  if (el.tagName === "A") {
    const href = el.getAttribute("href");
    if (href && !isJsHref(href)) {
      return cap(el.href || href, CARD_HREF_CAP);
    }
  }
  return undefined;
}

function isJsHref(href) {
  return String(href || "")
    .trim()
    .toLowerCase()
    .indexOf("javascript:") === 0;
}

function mapRole(el) {
  const explicit = (el.getAttribute("role") || "").toLowerCase();
  const mapped = roleName(explicit);
  if (mapped) {
    return mapped;
  }
  const tag = el.tagName;
  if (tag === "A") {
    return "Hyperlink";
  }
  if (tag === "BUTTON") {
    return "Button";
  }
  if (tag === "SELECT") {
    return "ComboBox";
  }
  if (tag === "TEXTAREA") {
    return "Edit";
  }
  if (tag === "SUMMARY") {
    return "Button";
  }
  if (tag === "LABEL") {
    return "Text";
  }
  if (tag === "LI") {
    return "ListItem";
  }
  if (tag === "INPUT") {
    const t = String(el.type || "text").toLowerCase();
    if (t === "checkbox") {
      return "CheckBox";
    }
    if (t === "radio") {
      return "RadioButton";
    }
    if (t === "submit" || t === "button" || t === "reset" || t === "image") {
      return "Button";
    }
    return "Edit";
  }
  if (el.isContentEditable) {
    return "Edit";
  }
  return "Other";
}

function roleName(role) {
  switch (role) {
    case "button":
      return "Button";
    case "link":
      return "Hyperlink";
    case "tab":
      return "TabItem";
    case "menuitem":
      return "MenuItem";
    case "checkbox":
      return "CheckBox";
    case "radio":
      return "RadioButton";
    case "textbox":
      return "Edit";
    case "combobox":
      return "ComboBox";
    case "listitem":
      return "ListItem";
    case "option":
      return "ListItem";
    case "slider":
      return "Slider";
    case "treeitem":
      return "TreeItem";
    default:
      return "";
  }
}

function windowMetrics() {
  return {
    screenX: window.screenX,
    screenY: window.screenY,
    outerWidth: window.outerWidth,
    outerHeight: window.outerHeight,
    innerWidth: window.innerWidth,
    innerHeight: window.innerHeight,
    devicePixelRatio: window.devicePixelRatio,
  };
}

function mainText() {
  const main = document.querySelector("main");
  const raw = ((main && main.innerText) || (document.body && document.body.innerText) || "")
    .replace(/\s+\n/g, "\n")
    .trim();
  return cap(raw, MAIN_TEXT_CAP);
}

function collectCards() {
  const seen = [];
  const out = [];
  const nodes = document.querySelectorAll("article,[role=\"listitem\"],li,a[href]");
  for (let i = 0; i < nodes.length && out.length < CARD_CAP; i += 1) {
    const el = nodes[i];
    if (!includeNode(el) || isPassword(el)) {
      continue;
    }
    if (seen.indexOf(el) !== -1) {
      continue;
    }
    seen.push(el);
    const tag = el.tagName;
    const role = (el.getAttribute("role") || "").toLowerCase();
    const text = (el.innerText || "").replace(/\s+/g, " ").trim();
    const priced = PRICE_RE.test(text);
    const listing = tag === "ARTICLE" || role === "listitem";
    if (!listing && !priced) {
      continue;
    }
    if (!priced && listing && !text) {
      continue;
    }
    const href = cardHref(el);
    if (!href) {
      continue;
    }
    const rect = cssRect(el);
    if (!rect) {
      continue;
    }
    const price = cap(extractPrice(text), CARD_PRICE_CAP);
    const title = cap(cardTitle(el, text, price), CARD_TITLE_CAP);
    if (!title) {
      continue;
    }
    const card = {
      title: title,
      price: price,
      href: cap(href, CARD_HREF_CAP),
      rectCss: rect,
    };
    const miles = cardMiles(el, text);
    if (miles) card.miles = miles;
    const dealer = cardDealer(el);
    if (dealer) card.dealer = dealer;
    const distance = cardDistance(text);
    if (distance) card.distance = distance;
    const ofText = cardOf(text);
    if (ofText) card.of = ofText;
    out.push(card);
  }
  return out;
}

function itempropText(el, name) {
  const node = el.querySelector("[itemprop=\"" + name + "\"]");
  if (!node) {
    return "";
  }
  const content = attr(node, "content");
  if (content) {
    return content;
  }
  return (node.innerText || "").replace(/\s+/g, " ").trim();
}

function cardMiles(el, text) {
  const item = itempropText(el, "mileageFromOdometer");
  if (item) {
    return cap(item, CARD_MILES_CAP);
  }
  const data = attr(el, "data-mileage");
  if (data) {
    return cap(data, CARD_MILES_CAP);
  }
  const m = text.match(/(\d{1,3}(?:,\d{3})+|\d{4,})\s*(mi|miles)\b(?!\s*away)/i);
  return m ? cap(m[0].replace(/\s+/g, " ").trim(), CARD_MILES_CAP) : "";
}

function cardDealer(el) {
  const item = itempropText(el, "seller");
  if (item) {
    return cap(item, CARD_DEALER_CAP);
  }
  const data = attr(el, "data-dealer");
  if (data) {
    return cap(data, CARD_DEALER_CAP);
  }
  return "";
}

function cardDistance(text) {
  const away = text.match(/\d[\d,]*\s*(mi|miles)\s+away\b/i);
  if (away) {
    return cap(away[0].replace(/\s+/g, " ").trim(), CARD_DISTANCE_CAP);
  }
  const ship = text.match(/shipping from\b[^.\n]*/i);
  if (ship) {
    return cap(ship[0].replace(/\s+/g, " ").trim(), CARD_DISTANCE_CAP);
  }
  return "";
}

function cardOf(text) {
  const m = text.match(/\b(\d+)\s+of\s+(\d+)\b/i);
  return m ? cap(m[1] + " of " + m[2], CARD_OF_CAP) : "";
}

function collectListingMeta() {
  const heading = document.querySelector("h1,h2,[role=\"heading\"]");
  const headingText = heading
    ? (heading.innerText || "").replace(/\s+/g, " ").trim()
    : "";
  const text = (headingText + "\n" + mainText()).trim();
  const meta = {};
  let zip = "";
  let radius = "";
  const params = new URLSearchParams(location.search || "");
  zip = (params.get("zip") || "").trim();
  const maxDist = (params.get("maximum_distance") || "").trim();
  if (maxDist) {
    radius = maxDist + " mi";
  }
  const within = text.match(
    /within\s+(\d[\d,]*)\s*(mi|miles)\s+of\s+(\d{5}(?:-\d{4})?)/i
  );
  if (within) {
    if (!radius) {
      radius = within[1] + " mi";
    }
    if (!zip) {
      zip = within[3];
    }
  }
  if (zip) {
    meta.zip = cap(zip, ZIP_CAP);
  }
  if (radius) {
    meta.radius = cap(radius, RADIUS_CAP);
  }
  const countSrc = headingText || text;
  const count = parseResultCount(countSrc);
  if (count) {
    meta.result_count = count;
  }
  const local = parseLocalMatches(text);
  if (local && local !== count) {
    meta.local_matches = local;
  }
  const empty = parseEmptyState(text);
  if (empty) {
    meta.empty_state = empty;
  }
  return meta;
}

function parseResultCount(text) {
  const m = text.match(/(\d[\d,]*)\+?\s+(matches|cars|results)\b/i);
  if (!m) {
    return "";
  }
  return cap(m[0].replace(/\s+/g, " ").trim(), RESULT_COUNT_CAP);
}

function parseLocalMatches(text) {
  const m = text.match(/\b(\d+)\s+local\b/i);
  return m ? cap(m[1] + " local", LOCAL_MATCHES_CAP) : "";
}

function parseEmptyState(text) {
  const phrases = [
    "nothing fits those filters",
    "we couldn't find",
    "we couldnt find",
    "we couldn\u2019t find",
    "0 matches",
    "no cars match",
    "no results",
    "try a larger radius",
    "expand your search",
  ];
  const lower = text.toLowerCase();
  for (let i = 0; i < phrases.length; i += 1) {
    const idx = indexOfEmptyPhrase(lower, phrases[i]);
    if (idx !== -1) {
      return cap(text.slice(idx, idx + phrases[i].length), EMPTY_STATE_CAP);
    }
  }
  return "";
}

function indexOfEmptyPhrase(lower, phrase) {
  let search = 0;
  while (search <= lower.length) {
    const idx = lower.indexOf(phrase, search);
    if (idx === -1) {
      return -1;
    }
    const before = idx === 0 || !/\d/.test(lower.charAt(idx - 1));
    const afterCh = lower.charAt(idx + phrase.length);
    const after = !afterCh || !/[A-Za-z0-9]/.test(afterCh);
    if (before && after) {
      return idx;
    }
    search = idx + 1;
  }
  return -1;
}

function cardHref(el) {
  if (el.tagName === "A") {
    const href = el.getAttribute("href");
    if (href && !isJsHref(href)) {
      return el.href || href;
    }
    return "";
  }
  const a = el.querySelector("a[href]");
  if (a) {
    const href = a.getAttribute("href");
    if (href && !isJsHref(href)) {
      return a.href || href;
    }
  }
  return "";
}

function extractPrice(text) {
  const m = text.match(/\$[\d,]+(?:\.\d{2})?|€[\d,]+(?:\.\d{2})?|£[\d,]+(?:\.\d{2})?|\d[\d,]*\.\d{2}/);
  return m ? m[0] : "";
}

function cardTitle(el, text, price) {
  const heading = el.querySelector("h1,h2,h3,h4,[role=\"heading\"]");
  if (heading) {
    const t = (heading.innerText || "").replace(/\s+/g, " ").trim();
    if (t) {
      return t;
    }
  }
  const aria = attr(el, "aria-label");
  if (aria) {
    return aria;
  }
  if (!text) {
    return "";
  }
  if (price && text.indexOf(price) !== -1) {
    return text.replace(price, "").replace(/\s+/g, " ").trim();
  }
  return text;
}

function cap(s, n) {
  const t = String(s || "");
  if (t.length <= n) {
    return t;
  }
  return t.slice(0, n);
}
