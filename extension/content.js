const TEXT_CAP = 80;
const MAIN_TEXT_CAP = 2000;
const CARD_TITLE_CAP = 80;
const CARD_PRICE_CAP = 24;
const CARD_HREF_CAP = 200;
const CARD_CAP = 8;
const CARD_WALK_CAP = 24;
const CARD_MILES_CAP = 16;
const CARD_DEALER_CAP = 48;
const CARD_DISTANCE_CAP = 40;
const CARD_OF_CAP = 12;
const CARD_KIND_CAP = 12;
const CARD_DELIVERY_CAP = 40;
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
  const collected = collectCards();
  const snap = {
    url: location.href || "",
    title: document.title || "",
    main_text: mainText(),
    elements: elements,
    cards: collected.cards,
    metrics: metrics,
  };
  if (collected.cards_walked > 0) {
    snap.cards_walked = collected.cards_walked;
  }
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
  for (let i = 0; i < nodes.length && out.length < CARD_WALK_CAP; i += 1) {
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
    const dealer = cardDealer(el, text, title, price);
    if (dealer) card.dealer = dealer;
    const distance = cardDistance(text);
    if (distance) card.distance = distance;
    const ofText = cardOf(text);
    if (ofText) card.of = ofText;
    const delivery = cardDelivery(text);
    if (delivery) card.delivery = delivery;
    const kind = cardKind(text, card);
    if (kind) card.kind = kind;
    out.push(card);
  }
  const cards_walked = out.length;
  const packed = preferLocalCards(out, CARD_CAP);
  return { cards: packed, cards_walked: cards_walked };
}

function preferLocalCards(cards, cap) {
  const local = [];
  const unknown = [];
  const ship = [];
  const rec = [];
  for (let i = 0; i < cards.length; i += 1) {
    const k = cards[i].kind;
    if (k === "local") {
      local.push(cards[i]);
    } else if (k === "ship") {
      ship.push(cards[i]);
    } else if (k === "recommended") {
      rec.push(cards[i]);
    } else {
      unknown.push(cards[i]);
    }
  }
  return local.concat(unknown, ship, rec).slice(0, cap);
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

function cardDealer(el, text, title, price) {
  const item = itempropText(el, "seller");
  if (item) {
    return sanitizeDealer(item);
  }
  const data = attr(el, "data-dealer");
  if (data) {
    return sanitizeDealer(data);
  }
  let rest = String(text || "");
  if (title) {
    rest = rest.split(title).join(" ");
  }
  if (price) {
    rest = rest.split(price).join(" ");
  }
  rest = rest.replace(/(\d{1,3}(?:,\d{3})+|\d{4,})\s*(mi|miles)\b(?!\s*away)/ig, " ");
  const distance = cardDistance(rest);
  if (distance) {
    rest = rest.split(distance).join(" ");
  }
  let delivery = cardDelivery(rest);
  while (delivery) {
    rest = rest.split(delivery).join(" ");
    delivery = cardDelivery(rest);
  }
  rest = rest.replace(/\b\d+\s+of\s+\d+\b/ig, " ");
  rest = rest.replace(/\(\s*[\d,]+\s+reviews?\)/ig, " ");
  rest = rest.replace(/[$€£][\d,]+(?:\.\d{2})?/g, " ");
  const junk = [
    "american-made index",
    "american made index",
    "available for delivery",
    "no price analysis",
    "get pre-approved",
    "check availability",
    "days on cars.com",
    "you may also like",
    "contact dealer",
    "home delivery",
    "est. shipping",
    "free carfax",
    "view details",
    "get financing",
    "price drop",
    "great deal",
    "good deal",
    "fair deal",
    "high price",
    "deliver to",
    "shipping to",
    "per month",
    "see more",
    "recommended",
    "autocheck",
    "sponsored",
    "shipping",
    "reviews",
    "ratings",
    "review",
    "rating",
    "stars",
    "star",
    "est.",
    "/mo",
  ];
  for (let i = 0; i < junk.length; i += 1) {
    rest = stripJunkPhrase(rest, junk[i]);
  }
  rest = rest.replace(/\s+/g, " ").trim();
  rest = rest.replace(/^[^A-Za-z0-9]+|[^A-Za-z0-9]+$/g, "");
  return sanitizeDealer(rest);
}

function sanitizeDealer(rest) {
  rest = String(rest || "");
  rest = rest.replace(/\(\s*[\d,]+\s+reviews?\)/ig, " ");
  const junk = [
    "american-made index",
    "american made index",
    "available for delivery",
    "no price analysis",
    "get pre-approved",
    "check availability",
    "days on cars.com",
    "you may also like",
    "contact dealer",
    "home delivery",
    "est. shipping",
    "free carfax",
    "view details",
    "get financing",
    "price drop",
    "great deal",
    "good deal",
    "fair deal",
    "high price",
    "deliver to",
    "shipping to",
    "per month",
    "see more",
    "recommended",
    "autocheck",
    "sponsored",
    "shipping",
    "reviews",
    "ratings",
    "review",
    "rating",
    "stars",
    "star",
    "est.",
    "/mo",
  ];
  for (let i = 0; i < junk.length; i += 1) {
    rest = stripJunkPhrase(rest, junk[i]);
  }
  rest = rest.replace(/[$€£][\d,]+(?:\.\d{2})?/g, " ");
  rest = rest.replace(/\s+/g, " ").trim();
  rest = rest.replace(/^[^A-Za-z0-9]+|[^A-Za-z0-9]+$/g, "");
  if (rest.length < 2 || !/[A-Za-z]/.test(rest)) {
    return "";
  }
  const exact = /^(used|new|save|view|details|more)$/i;
  if (exact.test(rest)) {
    return "";
  }
  const lower = rest.toLowerCase();
  for (let i = 0; i < junk.length; i += 1) {
    if (junkHasPhrase(lower, junk[i])) {
      return "";
    }
  }
  const tokens = rest.split(/\s+/);
  if (tokens.length === 1 && !isDealerishToken(tokens[0])) {
    return "";
  }
  return cap(rest, CARD_DEALER_CAP);
}

function stripJunkPhrase(text, phrase) {
  const lower = text.toLowerCase();
  const p = phrase.toLowerCase();
  let search = 0;
  let out = "";
  let last = 0;
  while (search <= lower.length) {
    const idx = lower.indexOf(p, search);
    if (idx === -1) {
      break;
    }
    if (p === "/mo") {
      out += text.slice(last, idx) + " ";
      last = idx + p.length;
      search = idx + p.length;
      continue;
    }
    const before = idx === 0 || !/[A-Za-z0-9]/.test(lower.charAt(idx - 1));
    const afterCh = lower.charAt(idx + p.length);
    const after = !afterCh || !/[A-Za-z0-9]/.test(afterCh);
    if (before && after) {
      out += text.slice(last, idx) + " ";
      last = idx + p.length;
      search = idx + p.length;
    } else {
      search = idx + 1;
    }
  }
  return out + text.slice(last);
}

function junkHasPhrase(lower, phrase) {
  const p = phrase.toLowerCase();
  if (p === "/mo") {
    return lower.indexOf(p) !== -1;
  }
  let search = 0;
  while (search <= lower.length) {
    const idx = lower.indexOf(p, search);
    if (idx === -1) {
      return false;
    }
    const before = idx === 0 || !/[A-Za-z0-9]/.test(lower.charAt(idx - 1));
    const afterCh = lower.charAt(idx + p.length);
    const after = !afterCh || !/[A-Za-z0-9]/.test(afterCh);
    if (before && after) {
      return true;
    }
    search = idx + 1;
  }
  return false;
}

function isDealerishToken(tok) {
  const l = String(tok || "").toLowerCase();
  return l === "carmax" || l === "carvana" || l === "vroom" || l.indexOf("auto") !== -1 || l.indexOf("motor") !== -1;
}

function cardDistance(text) {
  const paren = text.match(/\(\s*\d{1,3}\s*(mi|miles)\)/i);
  if (paren) {
    const at = text.indexOf(paren[0]);
    const before = text.slice(0, at);
    const city = before.match(/([A-Za-z][A-Za-z0-9]*)\s*,\s*([A-Za-z]{2})\s*$/);
    if (city) {
      return cap((city[0] + paren[0]).replace(/\s+/g, " ").trim(), CARD_DISTANCE_CAP);
    }
    return cap(paren[0].replace(/\s+/g, " ").trim(), CARD_DISTANCE_CAP);
  }
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

function cardDelivery(text) {
  const avail = text.match(/available for delivery to\b[^.\n]*/i);
  if (avail) {
    return cap(avail[0].replace(/\s+/g, " ").trim(), CARD_DELIVERY_CAP);
  }
  const est = text.match(/est\.?\s*shipping\b[^.\n]*/i);
  if (est) {
    return cap(est[0].replace(/\s+/g, " ").trim(), CARD_DELIVERY_CAP);
  }
  const shipFee = text.match(/shipping\s+[$€£][\d,]+(?:\.\d{2})?/i);
  if (shipFee && !/^shipping from\b/i.test(shipFee[0])) {
    return cap(shipFee[0].replace(/\s+/g, " ").trim(), CARD_DELIVERY_CAP);
  }
  const deliver = text.match(/deliver to\s+\d{5}(?:-\d{4})?/i);
  if (deliver) {
    return cap(deliver[0].replace(/\s+/g, " ").trim(), CARD_DELIVERY_CAP);
  }
  return "";
}

function cardKind(text, card) {
  const lower = String(text || "").toLowerCase();
  if (
    lower.indexOf("you may also like") !== -1 ||
    lower.indexOf("outside your search") !== -1 ||
    lower.indexOf("outside your area") !== -1 ||
    junkHasPhrase(lower, "recommended")
  ) {
    return cap("recommended", CARD_KIND_CAP);
  }
  const delivery = (card && card.delivery) || "";
  const distance = (card && card.distance) || "";
  if (
    /shipping from\b/i.test(distance) ||
    /shipping from\b/i.test(delivery) ||
    /shipping from\b/i.test(text) ||
    /shipping\s+[$€£]/i.test(text) ||
    /shipping\s+[$€£]/i.test(delivery) ||
    /deliver to\b/i.test(text) ||
    /deliver to\b/i.test(delivery)
  ) {
    return cap("ship", CARD_KIND_CAP);
  }
  if (/\bmi\s+away\b/i.test(text) || /\bmiles\s+away\b/i.test(text) || /\(\s*\d{1,3}\s*(mi|miles)\)/i.test(text)) {
    return cap("local", CARD_KIND_CAP);
  }
  if (/\bmi\s+away\b/i.test(distance) || /\(\s*\d{1,3}\s*(mi|miles)\)/i.test(distance)) {
    return cap("local", CARD_KIND_CAP);
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
  if (/^\d+$/.test(maxDist)) {
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
  if (!radius && /^all$/i.test(maxDist)) {
    radius = "all";
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
      let start = 0;
      for (let j = idx - 1; j >= 0; j -= 1) {
        if (".!?\n\r".indexOf(text.charAt(j)) !== -1) {
          start = j + 1;
          break;
        }
      }
      const afterPhrase = idx + phrases[i].length;
      let end = text.length;
      for (let j = afterPhrase; j < text.length; j += 1) {
        if (".!?\n\r".indexOf(text.charAt(j)) !== -1) {
          end = j + 1;
          break;
        }
      }
      const sentence = text.slice(start, end).trim();
      return sentence ? cap(sentence, EMPTY_STATE_CAP) : "";
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
  const skipTok = { est: 1, "est.": 1, drop: 1, save: 1, off: 1, discount: 1 };
  const hits = [];
  const re = /[$€£][\d,]+(?:\.\d{2})?/g;
  let m = re.exec(text);
  while (m) {
    const start = m.index;
    const end = start + m[0].length;
    const after = text.slice(end).replace(/^\s+/, "");
    let skip = false;
    if (after.indexOf("/mo") === 0 || /^\/\s*mo\b/i.test(after) || /^\bmo\b/i.test(after)) {
      skip = true;
    }
    const prev = prevWholeToken(text, start);
    const next1 = nextWholeToken(text, end);
    const next2 = next1 ? nextWholeToken(text, next1.end) : null;
    if (prev && skipTok[prev.toLowerCase()]) {
      skip = true;
    }
    if (next1 && skipTok[next1.tok.toLowerCase()]) {
      skip = true;
    }
    if (next2 && skipTok[next2.tok.toLowerCase()]) {
      skip = true;
    }
    if (!skip) {
      hits.push({ tok: m[0], grouped: m[0].indexOf(",") !== -1 });
    }
    m = re.exec(text);
  }
  for (let i = 0; i < hits.length; i += 1) {
    if (hits[i].grouped) {
      return hits[i].tok;
    }
  }
  if (hits.length) {
    return hits[0].tok;
  }
  const dec = text.match(/\d[\d,]*\.\d{2}/);
  return dec ? dec[0] : "";
}

function prevWholeToken(text, pos) {
  let i = pos;
  while (i > 0 && /\s/.test(text.charAt(i - 1))) {
    i -= 1;
  }
  if (i === 0) {
    return "";
  }
  if (text.charAt(i - 1) === ".") {
    let s = i - 1;
    while (s > 0 && /[A-Za-z0-9]/.test(text.charAt(s - 1))) {
      s -= 1;
    }
    return text.slice(s, i);
  }
  let s = i;
  while (s > 0 && /[A-Za-z0-9]/.test(text.charAt(s - 1))) {
    s -= 1;
  }
  return text.slice(s, i);
}

function nextWholeToken(text, pos) {
  let i = pos;
  while (i < text.length && /\s/.test(text.charAt(i))) {
    i += 1;
  }
  if (i >= text.length || !/[A-Za-z0-9]/.test(text.charAt(i))) {
    return null;
  }
  let end = i;
  while (end < text.length && /[A-Za-z0-9]/.test(text.charAt(end))) {
    end += 1;
  }
  if (text.charAt(end) === "." && text.slice(i, end + 1).toLowerCase() === "est.") {
    return { tok: text.slice(i, end + 1), end: end + 1 };
  }
  return { tok: text.slice(i, end), end: end };
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
