// injected.js: I-11 in-page API on globalThis.__sb, installed in isolated world "sb" in every
// frame (docs/05 B-4 #3, #4, #6, #8, B-5). Plain ES2020, no dependencies, no bundler.
//
// Return shapes (JSON):
//   walk({base, dirty}) -> {count}      ids start at `base` (sb-driver passes one per document
//                                       generation), so an id is never reused in a tab frame.
//                                       dirty: true re-walks only the MutationObserver dirty set.
//                                       The first walk must carry a base (D-405 (2)).
//   index()            -> IndexRow[]   exactly the serde shape of sb_sheet::IndexRow (I-5, D-37);
//                                      see crates/sb-sheet/src/index.rs test `index_row_wire_shape`.
//                                      JS emits frame 0 and frame-local ids; sb-driver rewrites
//                                      frame, ElemRefs and child_frame when it merges frames.
//                                      A challenge page is one CHALLENGE row (D-405 (3), D-424).
//   text(region)       -> TextBlock[]  sb_sheet::TextBlock, frame-local `order`
//   resolve(id)        -> element | {why: "gone" | "stale"}  ("stale": an id this document never
//                                      issued, below its base or before its first based walk;
//                                      "gone": issued here and lost)
//   act(id)            -> Promise<{x, y} | {why}>
//   settle(opts)       -> Promise<{settled, changed}>   opts {quiet, cap, content_ms}
//   fingerprint(spec)  -> "match" | {partial: [missing...]} | "miss"
//   frames()           -> [{frame, parent, url, opaque, ...}]
//   id(el), name(el), role(el), state(el): the single-element pieces of a row.
// Before the first walk({base}), id, index, text, fingerprint, state, name and frames issue no
// ids and return {why: "no base"} (D-405 (2)); role, resolve, act and settle work.
// skeleton / find / sheet are NOT in-page (D-51): sb-sheet scores the shipped index in Rust.
// Never touch window globals the site can see; never read a password value; never read
// inside a CHALLENGE frame.
(() => {
  "use strict";
  // addScriptToEvaluateOnNewDocument plus an evaluate into already-loaded documents can run
  // this twice in one world.
  if (globalThis.__sb) return;

  const doc = globalThis.document;
  if (!doc) return;

  // ---------------------------------------------------------------- constants
  const NAME_CAP = 150;
  const VALUE_CAP = 200;
  const OPTIONS_CAP = 25;
  const NEARBY_PX = 200;
  const NEARBY_CAP = 80;
  const QUIET_MS = 150;
  const CAP_MS = 3000;
  const DIRTY_MAX = 400;

  const HIDDEN = 0;
  const CONT = 1;

  const SKIP_TAGS = new Set([
    "script", "style", "noscript", "template", "head", "meta", "link", "title", "base",
  ]);
  // Elements whose children are never rows or text (replaced content, graphics).
  const LEAF_TAGS = new Set([
    "svg", "math", "canvas", "video", "audio", "img", "picture", "object", "embed", "input",
    "select", "textarea", "iframe", "frame",
  ]);
  // Roles that make a row. A row owns its subtree unless listed in NON_OWNING.
  const ROW_ROLES = new Set([
    "button", "link", "checkbox", "radio", "switch", "textbox", "searchbox", "combobox",
    "listbox", "slider", "spinbutton", "option", "menuitem", "menuitemcheckbox",
    "menuitemradio", "tab", "treeitem", "heading", "iframe", "row",
  ]);
  const NAME_FROM_CONTENT = new Set([
    "button", "link", "checkbox", "radio", "switch", "option", "menuitem", "menuitemcheckbox",
    "menuitemradio", "tab", "treeitem", "heading", "row", "cell", "gridcell",
  ]);
  const FIELD_ROLES = new Set(["textbox", "searchbox", "combobox", "listbox", "slider", "spinbutton"]);
  const CHECKABLE = new Set(["checkbox", "radio", "switch", "menuitemcheckbox", "menuitemradio"]);
  const BUTTONISH = new Set(["button"]);
  const INPUT_ROLE = {
    button: "button", submit: "button", reset: "button", image: "button", file: "button",
    color: "button", checkbox: "checkbox", radio: "radio", range: "slider",
    number: "spinbutton", search: "searchbox",
  };
  const NOVALUE_INPUT = new Set(["checkbox", "radio", "submit", "button", "reset", "image", "hidden"]);
  const LABELABLE = new Set(["input", "select", "textarea", "button", "meter", "output", "progress"]);
  const LANDMARK_ROLE = {
    main: "main", navigation: "nav", banner: "header", contentinfo: "footer",
    complementary: "aside", form: "form", search: "form", region: "region", dialog: "dialog",
    alertdialog: "dialog",
  };
  const BLOCK_TAGS = new Set([
    "address", "article", "aside", "blockquote", "body", "caption", "dd", "details", "dialog",
    "div", "dl", "dt", "fieldset", "figcaption", "figure", "footer", "form", "h1", "h2", "h3",
    "h4", "h5", "h6", "header", "hr", "legend", "li", "main", "nav", "ol", "p", "pre", "section",
    "summary", "table", "tbody", "thead", "tfoot", "tr", "td", "th", "ul", "html",
  ]);
  const INLINE_TAGS = new Set([
    "a", "abbr", "b", "bdi", "bdo", "cite", "code", "data", "dfn", "em", "i", "kbd", "mark",
    "q", "s", "samp", "small", "span", "strong", "sub", "sup", "time", "u", "var", "wbr", "font",
    "label", "del", "ins",
  ]);
  const INTERACTIVE_SEL =
    'a[href],button,input:not([type="hidden"]),select,textarea,iframe,summary,' +
    '[contenteditable=""],[contenteditable="true"],[role="button"],[role="link"],' +
    '[role="checkbox"],[role="radio"],[role="switch"],[role="textbox"],[role="searchbox"],' +
    '[role="combobox"],[role="tab"],[role="menuitem"],[role="option"],[onclick]';
  const VISIBLE_CONTROL_SEL = 'input:not([type="hidden"]),select,button,a[href]';

  // D-5: challenges are reported, never solved, never read inside.
  // A page or frame is a challenge by its host or path (D-405 (3)), never by a bare substring.
  const CHALLENGE_HOST_RE =
    /(^|\.)(challenges\.cloudflare\.com|recaptcha\.net|hcaptcha\.com|arkoselabs\.com|funcaptcha\.com|geetest\.com)$/i;
  const CHALLENGE_PATH_RE = /^\/cdn-cgi\/challenge-platform(\/|$)/i;
  const CHALLENGE_TITLE_PAGE_RE = /^just a moment(\.\.\.|…)?$/i;
  // Frame src / title / name / aria-label in the parent: the widget's own label.
  const CHALLENGE_FRAME_RE =
    /captcha|turnstile|cloudflare security challenge|verify you are human|human verification|verification challenge|arkose|funcaptcha|geetest/i;
  const CHALLENGE_BOX_RE = /captcha|turnstile|geetest|funcaptcha|arkose/i;
  // A human-check prompt on its own (D-422). "Press and hold" counts next to a human, robot or
  // bot word, or with challenge context (D-435 (3)); "are you (a) human" only with challenge
  // context (D-429 (2)): file pickers, voice buttons and quizzes say them. Challenge context is a
  // visible challenge widget, a response field or a challenge frame; passive reCAPTCHA v3
  // markers are not (D-436).
  const CHALLENGE_TEXT_RE =
    /verify (that )?you(['’]re| are) (a )?human|verify you are not a (ro)?bot|are you a robot|i(['’]m| am) not a robot|human verification/i;
  const HOLD_TEXT_RE = /press (and|&) hold/i;
  const HUMAN_WORD_RE = /\b(human|robot|bot)s?\b/i;
  const WEAK_TEXT_RE = /\bare you (a )?human\b/i;
  const CHALLENGE_TEXT_MAX = 200; // a prompt is short; a paragraph about captchas is not
  const RESPONSE_SEL =
    '[name="g-recaptcha-response"],[name="h-captcha-response"],[name="cf-turnstile-response"]';
  // Interactive or structural tags that are never a challenge widget box.
  const NOT_BOX_TAGS = new Set([
    "html", "body", "form", "main", "button", "input", "select", "textarea", "a", "label", "option",
  ]);
  const CONTENT_MS = 1000;
  const OVERLAY_RE =
    /cookie|consent|gdpr|modal|popup|pop-up|overlay|newsletter|subscribe|paywall|interstitial|lightbox/i;
  const SUBMIT_WORDS_RE =
    /^(submit|send|sign ?up|register|create (an |my |your )?account|join|log ?in|sign ?in|continue|next|save|confirm|apply|subscribe|place (your )?order|pay|buy|check ?out|book|reserve|post|publish|finish|done)\b/i;
  const SENSITIVE_AC_RE =
    /\b(cc-number|cc-csc|cc-exp|cc-exp-month|cc-exp-year|one-time-code|current-password|new-password)\b/;
  const SENSITIVE_NAME_RE =
    /pass(word|wd|code)?|pwd|\bpw\b|pw[_-]|secret|token|otp|cvc|cvv|csc|ssn|iban|card.?(num|no)|security.?code|\bpin\b/i;

  // In-page copy of the D-49 alias table (sb-sheet data/roles.txt), used only by fingerprint().
  // Keep it in step with data/roles.txt.
  const ALIASES = {
    input: ["textbox", "searchbox", "spinbutton"],
    textbox: ["textbox", "searchbox"],
    textarea: ["textbox"],
    field: ["textbox", "searchbox", "combobox", "listbox", "spinbutton", "slider", "checkbox", "radio", "switch"],
    search: ["searchbox", "textbox"],
    select: ["combobox", "listbox"],
    dropdown: ["combobox", "listbox"],
    combobox: ["combobox"],
    checkbox: ["checkbox", "switch", "menuitemcheckbox"],
    check: ["checkbox", "switch"],
    radio: ["radio", "menuitemradio"],
    switch: ["switch", "checkbox"],
    toggle: ["switch", "checkbox"],
    button: ["button"],
    btn: ["button"],
    link: ["link"],
    a: ["link"],
    iframe: ["iframe"],
    frame: ["iframe"],
    heading: ["heading"],
    h: ["heading"],
    tab: ["tab"],
    option: ["option"],
    menuitem: ["menuitem", "menuitemcheckbox", "menuitemradio"],
    slider: ["slider"],
    dialog: ["dialog", "alertdialog"],
    region: ["region"],
    row: ["row"],
  };

  // ---------------------------------------------------------------- frame state
  const S = {
    walked: false,
    based: false, // a walk({base}) has run: ids may be issued (D-405 (2))
    base: 1,
    next: 1,
    selfCh: false, // this document is a challenge page (set by each walk)
    sens: new WeakSet(), // elements ever seen as sensitive: only empty|filled, forever (D-405 (1))
    respBox: new WeakSet(), // parents of captcha response fields (D-405 (4))
    chCtx: false, // the document holds a challenge widget, response field or frame (D-429 (2))
    lastFill: new WeakMap(), // sensitive element -> last empty|filled evidence (D-426)
    gone: [], // [{ t, keys, parent }] removed sensitive field signatures, oldest first (D-435 (2)); keys null: match all; parent: WeakRef, D-439
    born: [], // [{ t, ref, parent }] recently added non-sensitive value fields, WeakRefs only (D-438 (1))
    ids: new WeakMap(), // Element -> id
    refs: new Map(), // id -> WeakRef<Element>
    rec: new WeakMap(), // Element -> walk decision (cached between walks)
    rows: [], // recs with a row, document order
    fresh: [], // recs computed in the current walk (post pass)
    consumed: new WeakSet(), // label elements / text nodes used as a name
    forms: new WeakMap(), // form -> "fN"
    formEls: new Map(), // "fN" -> WeakRef<form>
    formN: 0,
    tables: new WeakMap(),
    tableN: 0,
    dirty: new Set(),
    full: false,
    fullNow: false,
    layout: false,
    lm: new Map(), // per-walk landmark memo
    labels: [], // label elements seen by the current walk
    labelMap: null, // control -> {forL, wrapL}, built once per walk (el.labels is O(n) per call)
    observed: new WeakSet(),
    lastMut: 0,
    mutCount: 0,
    recent: [], // [t, node] of recently changed nodes, for settle's animation check
    waiters: new Set(),
  };

  const now = () => performance.now();

  // ---------------------------------------------------------------- small helpers
  const parentOf = (n) => {
    const p = n.parentNode;
    return p && p.nodeType === 11 && p.host ? p.host : p;
  };
  // Shadow-including containment: is b inside a (or a itself)?
  const contains = (a, b) => {
    for (let n = b; n; n = parentOf(n)) if (n === a) return true;
    return false;
  };
  const closestComposed = (el, sel) => {
    for (let n = el; n && n.nodeType === 1; n = parentOf(n)) {
      if (n.matches(sel)) return n;
    }
    return null;
  };
  const clean = (s, cap) => {
    if (!s) return "";
    let t = String(s).replace(/\s+/g, " ").trim();
    const c = cap || NAME_CAP;
    if (t.length > c) t = t.slice(0, c - 1).replace(/\s+\S*$/, "") + "…";
    return t;
  };
  const eqi = (a, b) => a.toLowerCase() === b.toLowerCase();
  const attr = (el, a) => {
    const v = el.getAttribute(a);
    return v == null ? "" : v;
  };
  const ariaRole = (el) => attr(el, "role").trim().toLowerCase().split(/\s+/)[0] || "";
  const style = (el) => getComputedStyle(el);
  const classOf = (el) => {
    const c = el.getAttribute("class");
    return c == null ? "" : c;
  };
  // jsdom has no layout: every box is 0x0 and elementFromPoint is missing.
  const hasLayout = () => {
    const de = doc.documentElement;
    if (!de || typeof doc.elementFromPoint !== "function") return false;
    const r = de.getBoundingClientRect();
    return r.width > 0 || r.height > 0;
  };
  const viewport = () => {
    const de = doc.documentElement;
    return {
      w: (de && de.clientWidth) || globalThis.innerWidth || 0,
      h: (de && de.clientHeight) || globalThis.innerHeight || 0,
    };
  };

  // ---------------------------------------------------------------- identity (B-4 #4)
  // Ids are issued only after the first walk({base}) (D-405 (2)), so every id this document
  // hands out is in [base, next) and an id outside that range was never issued here.
  const NO_BASE = () => ({ why: "no base" });

  function idOf(el) {
    if (!el || el.nodeType !== 1) return null;
    let id = S.ids.get(el);
    if (id === undefined) {
      if (!S.based) return null; // unreachable through the API, which checks S.based first
      id = S.next++;
      S.ids.set(el, id);
      S.refs.set(id, new WeakRef(el));
    }
    return id;
  }

  function idApi(el) {
    if (!S.based) return NO_BASE();
    return idOf(el);
  }

  function resolve(id) {
    const n = typeof id === "string" ? Number(id.replace(/^#/, "").replace(/^f\d+\./, "")) : id;
    if (!Number.isInteger(n)) return { why: "gone" };
    // Never issued by this document: from an earlier document generation.
    if (!S.based || n < S.base) return { why: "stale" };
    if (n >= S.next) return { why: "gone" };
    const ref = S.refs.get(n);
    const el = ref && ref.deref();
    if (!el || !el.isConnected) return { why: "gone" };
    return el;
  }

  function formId(f) {
    let id = S.forms.get(f);
    if (!id) {
      id = "f" + ++S.formN;
      S.forms.set(f, id);
      S.formEls.set(id, new WeakRef(f));
    }
    return id;
  }

  function formOf(el) {
    const f = el.form && el.form.nodeType === 1 ? el.form : closestComposed(el, "form");
    return f || null;
  }

  // ---------------------------------------------------------------- challenge (D-5, D-55)
  // A known challenge host or path (D-405 (3)).
  function challengeUrl(href) {
    let u;
    try {
      u = new URL(href, doc.baseURI);
    } catch (e) {
      return false;
    }
    if (u.protocol !== "http:" && u.protocol !== "https:") return false;
    if (CHALLENGE_HOST_RE.test(u.hostname)) return true;
    if (/(^|\.)google\.com$/i.test(u.hostname) && /^\/recaptcha\//i.test(u.pathname)) return true;
    return CHALLENGE_PATH_RE.test(u.pathname);
  }
  function challengeFrameEl(fr) {
    const src = attr(fr, "src");
    if (src && challengeUrl(src)) return true;
    return CHALLENGE_FRAME_RE.test(
      src + " " + attr(fr, "title") + " " + attr(fr, "name") + " " + attr(fr, "aria-label"),
    );
  }
  // reCAPTCHA v3 / invisible widgets have nothing for the user to solve (D-405 (5), B-402):
  // the badge, an invisible-size widget, and a challenge frame that is not visible (the
  // bframe stays hidden until a challenge is actually shown).
  function passiveBox(el) {
    if (/\bgrecaptcha-badge\b/.test(classOf(el))) return true;
    if (attr(el, "data-size").toLowerCase() === "invisible") return true;
    return !!closestComposed(el, ".grecaptcha-badge");
  }
  // D-438 (4): hCaptcha keeps its options (size=invisible included) in the hash of every frame,
  // so its challenge frame is never passive by URL, only by display; size=invisible counts only
  // in the query string of a reCAPTCHA frame (google.com/recaptcha/..., recaptcha.net).
  function passiveFrame(fr, vis) {
    if (!vis) return true;
    let u = null;
    try {
      u = new URL(attr(fr, "src"), doc.baseURI);
    } catch (e) { /* no usable src */ }
    if (u) {
      if (/(^|\.)hcaptcha\.com$/i.test(u.hostname) && /(^#|&)frame=challenge(&|$)/i.test(u.hash)) return false;
      const recaptcha = /(^|\.)recaptcha\.net$/i.test(u.hostname) ||
        (/(^|\.)google\.com$/i.test(u.hostname) && /^\/recaptcha\//i.test(u.pathname));
      if (recaptcha && /[?&]size=invisible(&|$)/i.test(u.search)) return true;
    }
    return !!closestComposed(fr, '.grecaptcha-badge,[data-size="invisible"]');
  }
  function challengeBox(el, vis) {
    const tag = el.localName;
    if (tag === "iframe" || tag === "frame") return challengeFrameEl(el) && !passiveFrame(el, vis);
    if (NOT_BOX_TAGS.has(tag)) return false;
    const named = CHALLENGE_BOX_RE.test(classOf(el) + " " + el.id) || el.hasAttribute("data-sitekey") ||
      el.hasAttribute("data-hcaptcha-widget-id") || S.respBox.has(el);
    if (!named || passiveBox(el)) return false;
    // A wrapper with real controls is not the widget (the controls stay rows).
    return !el.querySelector(VISIBLE_CONTROL_SEL);
  }
  function inChallenge(el) {
    for (let n = el; n && n.nodeType === 1; n = parentOf(n)) {
      const r = S.rec.get(n);
      if (r && r.challenge) return true;
      if (challengeBox(n, r ? r.vis : true)) return true;
    }
    return false;
  }
  // Short own text (direct text nodes and leaf inline children) that asks for a human check
  // (D-405 (4), D-422). Returns the text, or "".
  function challengeText(el) {
    let t = "";
    let any = false;
    for (let k = el.firstChild; k; k = k.nextSibling) {
      if (k.nodeType === 3) {
        t += k.data;
        if (!any && /\S/.test(k.data)) any = true;
      } else if (k.nodeType === 1 && INLINE_TAGS.has(k.localName) && k.childElementCount === 0) {
        t += k.textContent;
      }
      if (t.length > CHALLENGE_TEXT_MAX * 2) return "";
    }
    if (!any) return "";
    t = t.replace(/\s+/g, " ").trim();
    if (t.length < 8 || t.length > CHALLENGE_TEXT_MAX) return "";
    return challengeWords(t) ? t : "";
  }
  // Short text that asks for a human check (D-422, D-429 (2)).
  function challengeWords(t) {
    if (CHALLENGE_TEXT_RE.test(t)) return true;
    if (HOLD_TEXT_RE.test(t) && (S.chCtx || HUMAN_WORD_RE.test(t))) return true;
    return S.chCtx && WEAK_TEXT_RE.test(t);
  }
  // Challenge context for the weak prompts (D-429 (2), D-435 (3)): a response field, a visible
  // named widget box or a visible challenge frame in the light DOM, once per walk. Passive
  // reCAPTCHA v3 markers never count (D-436, D-405 (5), B-402): anything inside the badge or an
  // invisible-size widget, a v3-bound data-sitekey button, link, input or script, and widgets
  // or frames that are not displayed (the hidden bframe).
  const CTX_SEL = RESPONSE_SEL + ',[data-sitekey],[data-hcaptcha-widget-id],#px-captcha,' +
    '[class*="captcha" i],[id*="captcha" i],[class*="turnstile" i],[id*="turnstile" i],' +
    '[class*="geetest" i],[id*="arkose" i],[id*="funcaptcha" i]';
  const PASSIVE_SEL = '.grecaptcha-badge,[data-size="invisible"]';
  function challengeContext() {
    let list = [];
    try {
      list = doc.querySelectorAll(CTX_SEL);
    } catch (e) { /* an engine without the i flag: the frames below still count */ }
    for (const el of list) {
      if (closestComposed(el, PASSIVE_SEL)) continue;
      if (el.matches(RESPONSE_SEL)) return true; // response fields are hidden by design
      const tag = el.localName;
      if (NOT_BOX_TAGS.has(tag) || tag === "script") continue;
      if (displayed(el)) return true;
    }
    for (const fr of doc.querySelectorAll("iframe,frame")) {
      if (challengeFrameEl(fr) && !passiveFrame(fr, displayed(fr))) return true;
    }
    return false;
  }
  // Rendered and not visibility-hidden (itself or an ancestor). jsdom has no checkVisibility:
  // walk the ancestors' computed display and visibility instead.
  function displayed(el) {
    if (typeof el.checkVisibility === "function") {
      try {
        return el.checkVisibility({ visibilityProperty: true, checkVisibilityCSS: true });
      } catch (e) { /* fall through */ }
    }
    for (let n = el; n && n.nodeType === 1; n = parentOf(n)) {
      if (n.hidden) return false;
      const cs = style(n);
      if (cs.display === "none" || cs.visibility === "hidden" || cs.visibility === "collapse") return false;
    }
    return true;
  }
  // This document is a challenge page: a challenge host or path, the Cloudflare interstitial
  // title, or a frame whose owner element (same origin) is a challenge widget.
  function selfChallenge() {
    try {
      if (challengeUrl(String(globalThis.location && globalThis.location.href))) return true;
      if (CHALLENGE_TITLE_PAGE_RE.test(clean(doc.title))) return true;
      const fe = globalThis.frameElement;
      if (fe && (challengeBox(fe, true) || inChallenge(fe))) return true;
    } catch (e) {
      // frameElement across origins throws in some engines: not a challenge signal.
    }
    return false;
  }
  function scanResponseFields() {
    S.respBox = new WeakSet();
    for (const f of doc.querySelectorAll(RESPONSE_SEL)) {
      const p = parentOf(f);
      if (p && p.nodeType === 1) S.respBox.add(p);
    }
  }
  // The page's single row when it is itself a challenge (D-424). Nothing inside is read.
  function selfRow() {
    const de = doc.documentElement;
    let host = "";
    try {
      host = String(globalThis.location.hostname || "");
    } catch (e) {
      host = "";
    }
    const o = {
      id: idOf(de), frame: 0, role: "document", name: "challenge page" + (host ? " (" + host + ")" : ""),
      state: {}, flags: { challenge: true },
    };
    if (hasLayout()) {
      const vp = viewport();
      o.box = { x: 0, y: 0, w: vp.w, h: vp.h };
    }
    return o;
  }

  // ---------------------------------------------------------------- role (B-5 role)
  function clickable(el, cs) {
    const tag = el.localName;
    if (tag === "html" || tag === "body" || tag === "label" || tag === "option") return false;
    const handler = el.hasAttribute("onclick") || typeof el.onclick === "function";
    if (!handler) {
      if (cs.cursor !== "pointer") return false;
      const p = parentOf(el);
      if (p && p.nodeType === 1 && style(p).cursor === "pointer") return false;
    }
    if (el.querySelector(INTERACTIVE_SEL)) return false;
    if (!/\S/.test(el.textContent) && !el.querySelector("img,svg") && tag !== "img" && tag !== "svg" &&
      !el.getAttribute("aria-label") && !el.getAttribute("title")) return false;
    return true;
  }

  function roleOf(el, cs) {
    const aria = ariaRole(el);
    if (aria && aria !== "none" && aria !== "presentation" && aria !== "generic") return aria;
    const tag = el.localName;
    switch (tag) {
      case "a":
      case "area":
        if (el.hasAttribute("href")) return "link";
        break;
      case "button":
        return "button";
      case "input": {
        const t = String(el.type || "text").toLowerCase();
        if (t === "hidden") return "";
        return INPUT_ROLE[t] || "textbox";
      }
      case "select":
        return el.multiple || el.size > 1 ? "listbox" : "combobox";
      case "textarea":
        return "textbox";
      case "iframe":
      case "frame":
        return "iframe";
      case "summary":
        return "button";
      case "dialog":
        return el.open ? "dialog" : "";
      case "h1": case "h2": case "h3": case "h4": case "h5": case "h6":
        return "heading";
      case "nav":
        return "navigation";
      case "main":
        return "main";
      case "aside":
        return "complementary";
      case "form":
        return "form";
      case "img":
        if (clickable(el, cs || style(el))) return "button";
        return "img";
      default:
        break;
    }
    const ce = attr(el, "contenteditable").toLowerCase();
    if (el.hasAttribute("contenteditable") && (ce === "" || ce === "true" || ce === "plaintext-only")) {
      const p = parentOf(el);
      if (!(p && p.nodeType === 1 && p.isContentEditable)) return "textbox";
    }
    if (clickable(el, cs || style(el))) return tag === "tr" ? "row" : "button";
    return "";
  }

  // A role=combobox/textbox wrapper around a real field: the field is the row, not the wrapper.
  function wrapsField(el, role) {
    if (role !== "combobox" && role !== "textbox" && role !== "searchbox") return false;
    const tag = el.localName;
    if (tag === "input" || tag === "textarea" || tag === "select" || el.isContentEditable) return false;
    return !!el.querySelector("input:not([type=hidden]),textarea,select");
  }

  // ---------------------------------------------------------------- names (B-5 name ladder)
  // Visible text of a subtree for names. Never reads a field's value (passwords, H-2).
  function textOf(root, skip, cap) {
    const out = [];
    let len = 0;
    const max = cap || NAME_CAP * 2;
    const visit = (n, top) => {
      if (len > max) return;
      if (n.nodeType === 3) {
        out.push(n.data);
        len += n.data.length;
        return;
      }
      if (n.nodeType !== 1 || n === skip) return;
      const tag = n.localName;
      if (SKIP_TAGS.has(tag)) return;
      if (attr(n, "aria-hidden") === "true") return;
      if (n.hidden) return;
      if (!top) {
        const al = attr(n, "aria-label").trim();
        if (al) {
          out.push(" " + al + " ");
          len += al.length;
          return;
        }
      }
      if (tag === "img" || tag === "area") {
        out.push(" " + attr(n, "alt") + " ");
        return;
      }
      if (tag === "input" || tag === "select" || tag === "textarea") return;
      if (tag === "svg") {
        const t = n.querySelector("title");
        if (t) out.push(" " + t.textContent + " ");
        return;
      }
      if (tag === "br") {
        out.push(" ");
        return;
      }
      if (S.layout && !top && n.nodeType === 1) {
        const cs = style(n);
        if (cs.display === "none" || cs.visibility === "hidden") return;
      }
      const kids = n.shadowRoot ? n.shadowRoot.childNodes : n.childNodes;
      const blk = BLOCK_TAGS.has(tag);
      if (blk) out.push(" ");
      for (const k of kids) visit(k, false);
      if (blk) out.push(" ");
    };
    visit(root, true);
    return clean(out.join(""), cap || NAME_CAP);
  }

  // Labels of a control: from the per-walk map, else the DOM (single-element API calls).
  function labelsOf(el) {
    if (S.labelMap) return S.labelMap.get(el) || null;
    if (!el.labels || !el.labels.length) return null;
    const all = Array.from(el.labels);
    const forL = all.filter((l) => l.htmlFor && l.htmlFor === el.id);
    return { forL, wrapL: all.filter((l) => !forL.includes(l)) };
  }
  function buildLabelMap() {
    const m = new Map();
    for (const l of S.labels) {
      let c = null;
      try {
        c = l.control;
      } catch (e) {
        c = null;
      }
      if (!c) continue;
      let e = m.get(c);
      if (!e) m.set(c, (e = { forL: [], wrapL: [] }));
      if (l.htmlFor && l.htmlFor === c.id) e.forL.push(l);
      else e.wrapL.push(l);
    }
    S.labelMap = m;
  }

  function labelText(label, control) {
    return textOf(label, control);
  }

  function ownText(el, role) {
    const tag = el.localName;
    if (tag === "input") {
      const t = String(el.type).toLowerCase();
      if (t === "submit") return clean(attr(el, "value")) || "Submit";
      if (t === "reset") return clean(attr(el, "value")) || "Reset";
      if (t === "button") return clean(attr(el, "value"));
      if (t === "image") return clean(attr(el, "alt")) || clean(attr(el, "value")) || "Submit";
      return "";
    }
    if (tag === "select" || tag === "textarea") return "";
    if (role === "row") {
      const cells = cellsOf(el);
      return clean(cells.map((c) => textOf(c)).filter(Boolean).join(" | "), NAME_CAP * 2);
    }
    if (!NAME_FROM_CONTENT.has(role)) return "";
    return textOf(el);
  }

  function wordsRight(el) {
    let out = "";
    let n = el.nextSibling;
    let steps = 0;
    const used = [];
    while (n && steps++ < 8 && out.length < NEARBY_CAP) {
      if (n.nodeType === 3) {
        if (/\S/.test(n.data)) {
          out += " " + n.data;
          used.push(n);
        }
      } else if (n.nodeType === 1) {
        const t = n.localName;
        if (/^(input|select|textarea|button|br|div|p|li|tr|td|ul|ol|table|form|fieldset|section|h[1-6])$/.test(t)) break;
        out += " " + textOf(n);
        // A link inside the phrase stays its own row; its text is still emitted by text().
        if (!(t === "a" && n.hasAttribute("href"))) used.push(n);
      }
      n = n.nextSibling;
    }
    const s = clean(out);
    if (s) for (const u of used) S.consumed.add(u);
    return s;
  }

  function overlayName(el) {
    return clean(attr(el, "aria-label")) || clean(attr(el, "title")) ||
      (() => {
        const h = el.querySelector("h1,h2,h3,h4,[role=heading]");
        return h ? textOf(h) : "";
      })() || textOf(el, null, 60);
  }

  // Rungs 1..7. Nearby (rung 8) and the name attribute are applied in the post pass.
  function ladder(el, role) {
    let v;
    const lb = attr(el, "aria-labelledby").trim();
    if (lb) {
      const root = el.getRootNode();
      v = clean(lb.split(/\s+/).map((id) => {
        const r = root.getElementById ? root.getElementById(id) : doc.getElementById(id);
        if (!r) return "";
        const t = textOf(r, el);
        // A short non-heading referenced label is consumed like a <label> (D-412).
        if (t.length <= NEARBY_CAP && !/^h[1-6]$/.test(r.localName) && ariaRole(r) !== "heading" && !contains(r, el)) {
          S.consumed.add(r);
        }
        return t;
      }).join(" "));
      if (v) return v;
    }
    v = clean(attr(el, "aria-label"));
    if (v) return v;
    const tag = el.localName;
    const lab = LABELABLE.has(tag) ? labelsOf(el) : null;
    if (lab) {
      for (const set of [lab.forL, lab.wrapL]) {
        if (!set.length) continue;
        const t = clean(set.map((l) => labelText(l, el)).join(" "));
        if (t) {
          for (const l of set) S.consumed.add(l);
          return t;
        }
      }
    }
    const fromContent = NAME_FROM_CONTENT.has(role) || tag === "input";
    if (fromContent) {
      // accname: content before title for name-from-content roles (D-412).
      v = ownText(el, role);
      if (v) return v;
    }
    v = clean(attr(el, "placeholder")) || clean(attr(el, "title"));
    if (v) return v;
    if (tag === "img" || tag === "area" || (tag === "input" && el.type === "image")) {
      v = clean(attr(el, "alt"));
      if (v) return v;
    }
    if (role === "iframe") {
      return clean(attr(el, "name")) || hostOf(attr(el, "src")) || "frame";
    }
    if (role === "dialog" || role === "alertdialog" || role === "region") return overlayName(el);
    // Rung 7: words to the right of a checkbox or radio.
    const t = tag === "input" ? String(el.type).toLowerCase() : "";
    if (t === "checkbox" || t === "radio" || (CHECKABLE.has(role) && tag !== "input")) {
      v = wordsRight(el);
      if (v) return v;
    }
    return "";
  }

  function hostOf(src) {
    if (!src) return "";
    try {
      return new URL(src, doc.baseURI).hostname;
    } catch (e) {
      return "";
    }
  }

  // Last resort for an icon-only link: its last path segment ("/account/cart?x" -> "cart").
  function hrefName(href) {
    try {
      const u = new URL(href, doc.baseURI);
      const seg = u.pathname.split("/").filter(Boolean).pop() || u.hostname;
      return clean(decodeURIComponent(seg).replace(/\.[a-z0-9]+$/i, "").replace(/[-_+]+/g, " "), 60);
    } catch (e) {
      return "";
    }
  }

  // Text near the element (<= 200 px above or left) that is not consumed by a name.
  function nearbyOf(el, name) {
    const root = el.getRootNode();
    if (!root || !root.nodeType) return undefined;
    const tw = doc.createTreeWalker(root, NodeFilter.SHOW_TEXT);
    tw.currentNode = el;
    const r = S.layout ? el.getBoundingClientRect() : null;
    if (S.layout && (!r || (!r.width && !r.height))) return undefined;
    let t;
    let seen = 0;
    while ((t = tw.previousNode()) && seen++ < 40) {
      if (!/\S/.test(t.data) || S.consumed.has(t)) continue;
      const p = t.parentElement;
      if (!p || SKIP_TAGS.has(p.localName)) continue;
      if (blockedText(p)) continue;
      const txt = clean(t.data, NEARBY_CAP);
      if (!/[\p{L}\p{N}]/u.test(txt) || (name && eqi(txt, name))) continue;
      if (!S.layout) {
        // No layout (jsdom): the nearest preceding text inside the same parent.
        const par = el.parentElement;
        if (!par || !par.contains(t)) return undefined;
        return { text: txt, node: t };
      }
      const rg = doc.createRange();
      rg.selectNodeContents(t);
      const tr = rg.getBoundingClientRect();
      if (!tr || (!tr.width && !tr.height)) continue;
      const left = tr.right <= r.left + 2 && r.left - tr.right <= NEARBY_PX &&
        tr.bottom > r.top - 4 && tr.top < r.bottom + 4;
      const above = tr.bottom <= r.top + 2 && r.top - tr.bottom <= NEARBY_PX &&
        tr.left < r.right && tr.right > r.left - NEARBY_PX;
      if (left || above) return { text: txt, node: t };
      if (r.top - tr.bottom > NEARBY_PX * 2) break;
    }
    return undefined;
  }

  // Text inside another row, a consumed label, or a hidden subtree is not nearby text.
  function blockedText(p) {
    let depth = 0;
    for (let n = p; n && n.nodeType === 1 && depth++ < 14; n = parentOf(n)) {
      if (S.consumed.has(n)) return true;
      const rec = S.rec.get(n);
      if (rec) {
        if (rec.kind === HIDDEN) return true;
        if (rec.row && rec.skipKids) return true;
        if (n === p && !rec.vis) return true;
      }
      if (attr(n, "aria-hidden") === "true") return true;
    }
    return false;
  }

  // ---------------------------------------------------------------- state (B-5 state)
  function isDisabled(el) {
    try {
      if (el.matches(":disabled")) return true;
    } catch (e) {
      // :disabled unsupported on this node type
    }
    return !!closestComposed(el, '[aria-disabled="true"]');
  }

  // Computed -webkit-text-security other than none: a text field masked like a password.
  function masked(el) {
    try {
      const v = style(el).getPropertyValue("-webkit-text-security").trim().toLowerCase();
      if (v && v !== "none") return true;
    } catch (e) { /* no computed style */ }
    return /-webkit-text-security\s*:\s*(?!none\b)[a-z]/i.test(attr(el, "style"));
  }

  // Sticky (D-405 (1)): once an element is sensitive it stays sensitive for its lifetime, so a
  // show-password toggle or a type swap on focus never exposes the value. onMut also records
  // elements whose old type or autocomplete was sensitive.
  function sensitive(el) {
    if (S.sens.has(el)) return true;
    const s = String(el.type).toLowerCase() === "password" ||
      SENSITIVE_AC_RE.test(attr(el, "autocomplete").toLowerCase()) ||
      SENSITIVE_NAME_RE.test(attr(el, "name") + " " + el.id) ||
      masked(el);
    if (s) S.sens.add(el);
    return s;
  }

  // empty|filled without calling the value getter (H-2: passwords are never read from the DOM).
  // Evidence, in order: required + validity.valueMissing (exact); a caret or selection past 0
  // (insertText and programmatic sets leave it at the end); :placeholder-shown (exact when a
  // placeholder exists); validity flags that only a value can raise. No evidence: empty.
  // A type swap (show password) resets the caret, so the evidence taken under the previous
  // type (by a read or by the last edit) carries over until the next edit, D-426.
  function filledNoRead(el) {
    const type = String(el.type).toLowerCase();
    const ev = fillEvidence(el);
    if (ev) {
      S.lastFill.set(el, { ev, type });
      return ev;
    }
    const last = S.lastFill.get(el);
    return last && last.type !== type ? last.ev : "empty";
  }
  function fillEvidence(el) {
    try {
      if (el.required && el.validity) return el.validity.valueMissing ? "empty" : "filled";
    } catch (e) { /* no validity */ }
    try {
      if (typeof el.selectionEnd === "number" && el.selectionEnd > 0) return "filled";
    } catch (e) { /* type without selection API */ }
    try {
      if (el.hasAttribute("placeholder") && attr(el, "placeholder")) {
        return el.matches(":placeholder-shown") ? "empty" : "filled";
      }
    } catch (e) { /* selector unsupported */ }
    try {
      const v = el.validity;
      if (v && (v.tooShort || v.tooLong || v.patternMismatch || v.typeMismatch || v.badInput)) return "filled";
    } catch (e) { /* no validity */ }
    return "";
  }

  function fieldValue(el, role) {
    const tag = el.localName;
    if (tag === "input") {
      const t = String(el.type).toLowerCase();
      if (NOVALUE_INPUT.has(t)) return undefined;
      if (t === "file") return { kind: el.files && el.files.length ? "filled" : "empty" };
      if (sensitive(el)) return { kind: filledNoRead(el) };
      const v = el.value;
      return v ? { kind: "text", text: v.slice(0, VALUE_CAP) } : { kind: "empty" };
    }
    if (tag === "textarea") {
      if (sensitive(el)) return { kind: filledNoRead(el) };
      const v = el.value;
      return v ? { kind: "text", text: v.slice(0, VALUE_CAP) } : { kind: "empty" };
    }
    if (tag === "select") {
      const sel = Array.from(el.selectedOptions || []).map((o) => clean(o.label || o.text));
      const s = sel.filter(Boolean).join(", ");
      return s ? { kind: "text", text: s.slice(0, VALUE_CAP) } : { kind: "empty" };
    }
    if (role === "textbox" && el.isContentEditable) {
      const s = textOf(el, null, VALUE_CAP);
      return s ? { kind: "text", text: s } : { kind: "empty" };
    }
    if (role === "slider" || role === "spinbutton") {
      const s = clean(attr(el, "aria-valuetext") || attr(el, "aria-valuenow"), VALUE_CAP);
      return s ? { kind: "text", text: s } : undefined;
    }
    return undefined;
  }

  function checkedOf(el, role) {
    if (el.localName === "input" && (el.type === "checkbox" || el.type === "radio")) return !!el.checked;
    const ac = el.getAttribute("aria-checked") != null ? attr(el, "aria-checked") : attr(el, "aria-pressed");
    if (ac === "true" || ac === "mixed") return true;
    if (ac === "false") return false;
    if (CHECKABLE.has(role)) return false;
    return undefined;
  }

  function nativeSubmit(el) {
    const tag = el.localName;
    if (tag === "button") return String(el.type).toLowerCase() === "submit" && !!el.form;
    if (tag === "input") {
      const t = String(el.type).toLowerCase();
      return (t === "submit" || t === "image") && !!el.form;
    }
    return false;
  }

  function opensNewTab(el, submit) {
    const tag = el.localName;
    let tgt = "";
    if (tag === "a" || tag === "area") tgt = attr(el, "target");
    else if (submit) tgt = attr(el, "formtarget") || (el.form ? attr(el.form, "target") : "");
    if (tgt && !/^_(self|parent|top)$/i.test(tgt)) return true;
    const oc = attr(el, "onclick");
    return !!oc && /\bopen\s*\(/.test(oc);
  }

  // ---------------------------------------------------------------- landmarks
  function landmarkName(n) {
    const role = ariaRole(n);
    if (role) {
      if (LANDMARK_ROLE[role]) return LANDMARK_ROLE[role];
      if (role !== "none" && role !== "presentation") return "";
    }
    switch (n.localName) {
      case "main": return "main";
      case "nav": return "nav";
      case "aside": return "aside";
      case "dialog": return n.open ? "dialog" : "";
      case "header":
      case "footer": {
        const p = parentOf(n);
        if (p && p.nodeType === 1 && closestComposed(p, "article,aside,main,nav,section")) return "";
        return n.localName;
      }
      case "section":
        return hasAccName(n) ? "region" : "";
      case "form":
        // ARIA: a form with an accessible name is a form landmark (D-405 (7)).
        return hasAccName(n) ? "form" : "";
      case "search":
        return "form";
      default:
        return "";
    }
  }
  function hasAccName(n) {
    return /\S/.test(attr(n, "aria-label")) || /\S/.test(attr(n, "aria-labelledby")) ||
      /\S/.test(attr(n, "title"));
  }
  // Nearest enclosing landmark. An unnamed <form> is not one: rows carry it in `form` (D-414,
  // amended by D-405 (7)).
  function landmarkOf(el) {
    const path = [];
    let res = null;
    for (let n = el; n && n.nodeType === 1; n = parentOf(n)) {
      const m = S.lm.get(n);
      if (m !== undefined) {
        res = m;
        break;
      }
      path.push(n);
      const name = landmarkName(n);
      if (name) {
        res = { name, el: n };
        break;
      }
    }
    for (const p of path) S.lm.set(p, res);
    return res;
  }

  // ---------------------------------------------------------------- walk decisions
  const HIDDEN_REC = Object.freeze({ kind: HIDDEN, skipKids: true, vis: false, row: null });

  function newRow(el, role, rec, opts) {
    const row = {
      id: idOf(el),
      el,
      role,
      name: "",
      nearby: undefined,
      formEl: null,
      landmark: undefined,
      always: true,
      cand: false,
      isHeading: role === "heading",
      level: 0,
      challenge: false,
      submit: false,
      submitish: false,
      newTab: false,
      options: undefined,
      inputType: undefined,
      autocomplete: undefined,
      named: false,
    };
    Object.assign(row, opts || {});
    rec.row = row;
    S.fresh.push(rec);
    return row;
  }

  // Heading level: the tag's, else aria-level as an integer clamped to 1..6, default 2
  // (D-405 (6): TextBlock.level is a u8 and one bad value must not fail the whole read).
  function headingLevel(el) {
    const tag = el.localName;
    if (/^h[1-6]$/.test(tag)) return Number(tag[1]);
    const n = Math.trunc(Number(attr(el, "aria-level")));
    if (!Number.isFinite(n) || n < 1) return 2;
    return Math.min(6, n);
  }

  function compute(el) {
    const tag = el.localName;
    if (SKIP_TAGS.has(tag)) return HIDDEN_REC;
    if (tag === "input" && String(el.type).toLowerCase() === "hidden") return HIDDEN_REC;
    const cs = style(el);
    if (cs.display === "none" || cs.contentVisibility === "hidden" || (!S.layout && el.hidden)) {
      return HIDDEN_REC;
    }
    const vis = cs.visibility !== "hidden" && cs.visibility !== "collapse";
    const rec = {
      kind: CONT, skipKids: LEAF_TAGS.has(tag), vis, disp: cs.display, pos: cs.position,
      row: null, challenge: false,
    };

    // A CHALLENGE container owns its subtree and is one row, carried on its iframe (D-55).
    if (challengeBox(el, vis)) {
      rec.challenge = true;
      rec.skipKids = true;
      const fr = tag === "iframe" || tag === "frame" ? el : el.querySelector("iframe,frame");
      if (fr) {
        const row = newRow(fr, "iframe", rec, { challenge: true });
        row.name = ladder(fr, "iframe");
        row.named = true;
      } else {
        const row = newRow(el, ariaRole(el) || "group", rec, { challenge: true });
        row.name = clean(attr(el, "aria-label")) || clean(attr(el, "title")) || "captcha";
        row.named = true;
      }
      return rec;
    }

    const role = roleOf(el, cs);
    if (role === "iframe") {
      rec.skipKids = true;
      newRow(el, "iframe", rec);
      return rec;
    }

    // "Verify you are human" and friends in a visible block (D-405 (4), D-422): the block is a
    // CHALLENGE row. With at most two controls inside it is the widget and owns its subtree;
    // around more controls it only marks them (act refuses them, text() skips them).
    if (vis && tag !== "html" && tag !== "body") {
      const ct = challengeText(el);
      if (ct) {
        rec.challenge = true;
        rec.skipKids = el.querySelectorAll(VISIBLE_CONTROL_SEL).length <= 2;
        const r = ROW_ROLES.has(role) && role !== "row" ? role : ariaRole(el) || "group";
        const row = newRow(el, r, rec, { challenge: true });
        if (r === "heading") row.level = headingLevel(el);
        row.name = clean(attr(el, "aria-label")) || clean(ct);
        row.named = true;
        return rec;
      }
    }

    if (vis && ROW_ROLES.has(role) && !wrapsField(el, role) && !(role === "listbox" && tag !== "select")) {
      const row = newRow(el, role, rec);
      if (role === "heading") {
        row.level = headingLevel(el);
        // A heading that holds links keeps them as rows.
        rec.skipKids = !el.querySelector(INTERACTIVE_SEL);
      } else {
        rec.skipKids = true;
      }
    } else if (vis && (role === "dialog" || role === "alertdialog" || attr(el, "aria-modal") === "true")) {
      newRow(el, role === "alertdialog" ? "alertdialog" : "dialog", rec);
    }
    if (vis && (cs.position === "fixed" || cs.position === "sticky")) {
      if (rec.row) rec.row.cand = true;
      else newRow(el, ariaRole(el) || "region", rec, { cand: true, always: false });
      rec.sticky = cs.position === "sticky";
    }
    return rec;
  }

  // Post pass for rows computed in this walk: name rungs, nearby, the name attribute fallback,
  // semantic state. Runs after the whole tree so label consumption is known.
  function finishRow(rec) {
    const row = rec.row;
    const el = row.el;
    if (!row.named) row.name = ladder(el, row.role);
    const container = row.role === "dialog" || row.role === "alertdialog" || !row.always;
    if (!row.isHeading && row.role !== "iframe" && !row.challenge && !container) {
      const nb = nearbyOf(el, row.name);
      if (nb) {
        if (!row.name) {
          // Rung 8: nearby text names a field that has nothing else; it is consumed like a label.
          row.name = nb.text;
          S.consumed.add(nb.node);
        } else {
          row.nearby = nb.text;
        }
      }
    }
    if (!row.name && el.localName !== "a") row.name = clean(attr(el, "name"));
    if (!row.name && row.role === "link") row.name = hrefName(attr(el, "href"));
    if (!row.name && (container || row.cand)) row.name = overlayName(el);
    const tag = el.localName;
    row.formEl = formOf(el);
    const lm = landmarkOf(el);
    row.landmark = lm ? lm.name : undefined;
    row.lmEl = lm ? lm.el : null;
    row.submit = nativeSubmit(el);
    row.submitish = row.submit || (BUTTONISH.has(row.role) && SUBMIT_WORDS_RE.test(row.name));
    row.newTab = opensNewTab(el, row.submit);
    if (tag === "select") {
      row.options = Array.from(el.options).slice(0, OPTIONS_CAP).map((o) => clean(o.label || o.text, 60));
    }
    if (tag === "input") row.inputType = String(el.type).toLowerCase();
    // Register sensitivity at walk time, so a later type swap cannot expose the value (D-405 (1)).
    if (tag === "input" || tag === "textarea") sensitive(el);
    const ac = attr(el, "autocomplete").trim().toLowerCase();
    if (ac && ac !== "on" && ac !== "off") row.autocomplete = ac;
    if (!row.challenge) {
      if ((row.isHeading || row.role === "dialog" || row.role === "alertdialog") && challengeWords(row.name)) {
        row.challenge = true;
      } else if (/captcha/i.test(attr(el, "name") + " " + el.id) && tag !== "form") {
        row.challenge = true;
      }
      // act reads rec.challenge for the row and everything inside it.
      if (row.challenge) rec.challenge = true;
    }
  }

  // ---------------------------------------------------------------- walk (B-5 walk)
  function nextSkip(tw) {
    for (;;) {
      const s = tw.nextSibling();
      if (s) return s;
      if (!tw.parentNode()) return null;
    }
  }

  function decide(n, dirty) {
    let rec = S.rec.get(n);
    if (!rec || dirty) {
      rec = compute(n);
      S.rec.set(n, rec);
    }
    if (rec.row) S.rows.push(rec);
    if (n.localName === "form" && rec.kind !== HIDDEN) formId(n);
    else if (n.localName === "label" && rec.kind !== HIDDEN) S.labels.push(n);
    return rec;
  }

  function observeShadow(sr) {
    if (S.observed.has(sr)) return;
    S.observed.add(sr);
    try {
      mo.observe(sr, MO_OPTS);
    } catch (e) { /* closed or detached root */ }
  }

  // Visit el and its flat-tree subtree in document order. TreeWalker over the light tree;
  // shadow roots and slots recurse (they are separate trees).
  function visitEl(el, dirtyIn) {
    const tw = doc.createTreeWalker(el, NodeFilter.SHOW_ELEMENT);
    let n = el;
    let dirtyRoot = null;
    while (n) {
      if (dirtyRoot && !contains(dirtyRoot, n)) dirtyRoot = null;
      if (!dirtyIn && !dirtyRoot && S.dirty.has(n)) dirtyRoot = n;
      const d = dirtyIn || !!dirtyRoot;
      const rec = decide(n, d);
      let skip = rec.skipKids;
      if (!skip) {
        const sr = n.shadowRoot;
        if (sr) {
          observeShadow(sr);
          for (const k of Array.from(sr.children)) visitEl(k, d);
          skip = true;
        } else if (n.localName === "slot" && typeof n.assignedElements === "function") {
          const as = n.assignedElements();
          if (as.length) {
            for (const k of as) visitEl(k, d);
            skip = true;
          }
        }
      }
      n = skip ? nextSkip(tw) : tw.nextNode();
    }
  }

  function doWalk(incremental) {
    // Records the observer has not delivered yet (same task as the change) count now, so a
    // rebuilt password field is marked before anything reads it (D-429 (1)).
    flushMut();
    if (S.full) incremental = false;
    // A challenge widget that appears or goes away changes what the weak prompts mean.
    const ctx = challengeContext();
    if (ctx !== S.chCtx) incremental = false;
    S.chCtx = ctx;
    S.layout = hasLayout();
    S.lm = new Map();
    S.rows = [];
    S.fresh = [];
    S.labels = [];
    S.labelMap = null;
    if (!incremental) S.consumed = new WeakSet();
    if (incremental) {
      // A change inside a row that owns its subtree re-walks that row (its name may change).
      for (const d of Array.from(S.dirty)) {
        for (let a = parentOf(d); a && a.nodeType === 1; a = parentOf(a)) {
          const r = S.rec.get(a);
          if (r && (r.row && r.skipKids || r.challenge)) S.dirty.add(a);
        }
      }
    }
    const root = doc.documentElement;
    S.selfCh = selfChallenge();
    if (root && S.selfCh) idOf(root);
    if (root && !S.selfCh) {
      scanResponseFields();
      visitEl(root, !incremental);
    }
    buildLabelMap();
    for (const rec of S.fresh) finishRow(rec);
    S.fresh = [];
    S.dirty.clear();
    S.full = false;
    S.walked = true;
    if (!incremental) {
      for (const [id, ref] of S.refs) if (!ref.deref()) S.refs.delete(id);
    }
  }

  function walk(opts) {
    if (typeof opts === "boolean") opts = { dirty: opts };
    opts = opts || {};
    if (!S.based) {
      // The first walk adopts the base sb-driver gives this document generation; without one
      // no id is issued, so ids can never restart below a later base (D-405 (2)).
      // I-11: the base is an integer; a numeric string ("5") is not one.
      const b = opts.base;
      if (!(typeof b === "number" && Number.isSafeInteger(b) && b > 0)) return NO_BASE();
      S.base = b;
      S.next = b;
      S.based = true;
    }
    let incremental = !!opts.dirty && S.walked && !S.full;
    if (Array.isArray(opts.dirty)) {
      for (const id of opts.dirty) {
        const el = resolve(id);
        if (el && el.nodeType === 1) S.dirty.add(el);
      }
      incremental = S.walked && !S.full;
    }
    doWalk(incremental);
    return { count: S.selfCh ? 1 : S.rows.length };
  }

  // Callers check S.based first: ensure() never runs the first walk itself.
  function ensure() {
    flushMut();
    if (!S.walked) doWalk(false);
    else if (S.full) doWalk(false);
    else if (S.dirty.size) doWalk(true);
  }

  // ---------------------------------------------------------------- geometry
  // Centre of the first client rect that intersects the viewport, clipped to it.
  function centre(el, vpIn) {
    const vp = vpIn || viewport();
    const rects = el.getClientRects();
    for (const r of rects) {
      if (r.width <= 0 || r.height <= 0) continue;
      const x0 = Math.max(r.left, 0);
      const y0 = Math.max(r.top, 0);
      const x1 = Math.min(r.right, vp.w);
      const y1 = Math.min(r.bottom, vp.h);
      if (x1 - x0 < 1 || y1 - y0 < 1) continue;
      return { x: (x0 + x1) / 2, y: (y0 + y1) / 2 };
    }
    return null;
  }

  function deepHit(x, y) {
    let hit = doc.elementFromPoint(x, y);
    while (hit && hit.shadowRoot) {
      const inner = hit.shadowRoot.elementFromPoint(x, y);
      if (!inner || inner === hit) break;
      hit = inner;
    }
    return hit;
  }

  function hitsSelf(el, hit) {
    if (!hit) return false;
    if (hit === el || contains(el, hit)) return true;
    const lab = labelsOf(el);
    if (lab) for (const l of lab.forL.concat(lab.wrapL)) if (contains(l, hit)) return true;
    return false;
  }

  // The fixed/sticky container a hit belongs to, if any.
  function coverOf(hit) {
    for (let n = hit; n && n.nodeType === 1; n = parentOf(n)) {
      const r = S.rec.get(n);
      if (r && r.row && r.row.cand) return n;
      if (!r && n !== doc.body && n !== doc.documentElement) {
        const p = style(n).position;
        if (p === "fixed" || p === "sticky") return n;
      }
    }
    return null;
  }

  function describe(n) {
    if (!n) return "nothing";
    let d = n.localName;
    if (n.id) d += "#" + n.id;
    else {
      const c = classOf(n).trim().split(/\s+/)[0];
      if (c) d += "." + c;
    }
    return d;
  }

  function coverWhy(el, hit) {
    if (!hit) return "covered by nothing (outside the viewport)";
    if (contains(hit, el)) return "does not receive pointer events (hit " + describe(hit) + ")";
    const cov = coverOf(hit);
    if (cov) return "covered by " + describe(cov) + " (OVERLAY #" + idOf(cov) + ")";
    for (let n = hit, i = 0; n && n.nodeType === 1 && i < 6; n = parentOf(n), i++) {
      const id = S.ids.get(n);
      if (id !== undefined) return "covered by " + describe(hit) + " (#" + id + ")";
    }
    return "covered by " + describe(hit);
  }

  // ---------------------------------------------------------------- index (I-5 rows)
  function index() {
    if (!S.based) return NO_BASE();
    ensure();
    if (S.selfCh) return [selfRow()];
    S.layout = hasLayout();
    const L = S.layout;
    const vp = viewport();
    const vpArea = Math.max(1, vp.w * vp.h);
    const live = [];
    const coverers = new Set();
    for (const rec of S.rows) {
      const row = rec.row;
      const el = row.el;
      if (!el.isConnected) continue;
      let box;
      let visible = rec.vis;
      let covered = false;
      if (L) {
        const r = el.getBoundingClientRect();
        box = { x: Math.round(r.x), y: Math.round(r.y), w: Math.round(r.width), h: Math.round(r.height) };
        visible = visible && r.width > 0 && r.height > 0;
        if (visible && typeof el.checkVisibility === "function") {
          visible = el.checkVisibility({ visibilityProperty: true });
        }
        const inVp = r.right > 0 && r.bottom > 0 && r.left < vp.w && r.top < vp.h;
        if (visible && !row.cand && inVp) {
          const p = centre(el, vp);
          if (p) {
            const hit = deepHit(p.x, p.y);
            if (!hitsSelf(el, hit)) {
              covered = true;
              const cov = hit && coverOf(hit);
              if (cov) coverers.add(cov);
            }
          }
        }
      }
      live.push({ rec, row, el, box, visible, covered });
    }
    const out = [];
    const headings = [];
    for (const l of live) {
      const { row, el, box, visible } = l;
      let overlay = false;
      if (row.cand && L && visible && box) {
        const ix = Math.max(0, Math.min(box.x + box.w, vp.w) - Math.max(box.x, 0));
        const iy = Math.max(0, Math.min(box.y + box.h, vp.h) - Math.max(box.y, 0));
        const area = ix * iy;
        if (area > 0) {
          if (coverers.has(el)) overlay = true;
          else if (!l.rec.sticky) {
            overlay = area >= 0.25 * vpArea ||
              (area >= 0.02 * vpArea &&
                (OVERLAY_RE.test(classOf(el) + " " + el.id + " " + attr(el, "aria-label")) ||
                  row.role === "dialog" || row.role === "alertdialog" || attr(el, "aria-modal") === "true"));
          }
        }
      }
      if (!row.always && !overlay) continue;
      if (row.isHeading) headings.push({ name: row.name, lmEl: row.lmEl });
      // Fields equal to sb_sheet::State::default() are omitted (serde(default) fills them):
      // visible and enabled default to true, the other booleans to false.
      const st = {};
      if (!visible) st.visible = false;
      if (isDisabled(el)) st.enabled = false;
      if (row.submitish) st.submitish = true;
      if (row.newTab) st.opens_new_tab = true;
      if (l.covered) st.covered = true;
      const ck = checkedOf(el, row.role);
      if (ck !== undefined) st.checked = ck;
      if (!row.challenge) {
        const v = fieldValue(el, row.role);
        if (v) st.value = v;
      }
      if (row.options && row.options.length) st.options = row.options;
      if (row.inputType) st.input_type = row.inputType;
      if (row.autocomplete) st.autocomplete = row.autocomplete;
      const o = { id: row.id, frame: 0, role: row.role, name: row.name };
      if (row.nearby) o.nearby = row.nearby;
      if (!row.isHeading) {
        for (let i = headings.length - 1; i >= 0; i--) {
          const h = headings[i];
          if (!h.lmEl || contains(h.lmEl, el)) {
            if (h.name) o.heading = h.name;
            break;
          }
        }
      }
      o.state = st;
      const fl = {};
      if (row.submit) fl.submit = true;
      if (row.challenge) fl.challenge = true;
      if (overlay) fl.overlay = true;
      if (row.role === "iframe") {
        let opaque = false;
        try {
          opaque = el.contentDocument == null;
        } catch (e) {
          opaque = true;
        }
        if (opaque) fl.opaque = true;
      }
      o.flags = fl;
      if (row.formEl && row.formEl.isConnected) o.form = formId(row.formEl);
      if (row.landmark) o.landmark = row.landmark;
      if (box) o.box = box;
      out.push(o);
    }
    return out;
  }

  // ---------------------------------------------------------------- text (I-6 TextBlock)
  function cellsOf(tr) {
    return Array.from(tr.children).filter((c) => {
      const t = c.localName;
      const r = ariaRole(c);
      return t === "td" || t === "th" || r === "cell" || r === "gridcell" || r === "columnheader" || r === "rowheader";
    });
  }

  function tableId(t) {
    if (!t) return undefined;
    let id = S.tables.get(t);
    if (id === undefined) {
      id = ++S.tableN;
      S.tables.set(t, id);
    }
    return id;
  }

  function blockKind(el) {
    const tag = el.localName;
    if (/^h[1-6]$/.test(tag)) return ["heading", Number(tag[1])];
    if (tag === "p") return ["paragraph", 0];
    if (tag === "li") return ["list_item", 0];
    if (tag === "pre") return ["code", 0];
    if (tag === "blockquote") return ["quote", 0];
    if (tag === "dt" || tag === "dd" || tag === "figcaption" || tag === "caption" || tag === "legend") {
      return ["other", 0];
    }
    return ["paragraph", 0];
  }

  function isBlock(el, rec) {
    const tag = el.localName;
    if (BLOCK_TAGS.has(tag)) return true;
    if (INLINE_TAGS.has(tag)) return false;
    const d = rec && rec.disp;
    return !!d && !d.startsWith("inline") && d !== "contents" && d !== "none";
  }

  function text(region) {
    if (!S.based) return NO_BASE();
    ensure();
    S.lm = new Map();
    const blocks = [];
    if (S.selfCh) {
      // One block naming the challenge, linked to its row; nothing inside is read (D-424).
      const r = selfRow();
      return [{ order: 0, frame: 0, kind: "other", spans: [{ text: r.name, elem: "#" + r.id }] }];
    }
    let order = 0;
    const headings = [];
    const stack = []; // {kind, level, el}
    let cur = null; // {spans, el, kind, level, pre}

    const open = (el, kind, level, pre) => {
      cur = { spans: [], el, kind, level, pre };
    };
    const flush = () => {
      if (!cur) return;
      const spans = [];
      for (const s of cur.spans) {
        let t = cur.pre ? s.text : s.text.replace(/\s+/g, " ");
        const last = spans[spans.length - 1];
        if (s.elem) {
          // A linked span holds just its words; the space around it goes to plain text.
          const lead = /^\s/.test(t) || (last && last.elem);
          t = t.trim();
          if (lead && last) {
            if (last.elem) spans.push({ text: " " });
            else if (!/\s$/.test(last.text)) last.text += " ";
          }
          spans.push({ text: t, elem: s.elem, trail: /\s$/.test(s.text) });
        } else if (last && !last.elem) {
          last.text += t;
        } else {
          spans.push({ text: last && last.trail && !/^\s/.test(t) ? " " + t : t });
        }
      }
      if (spans.length && !cur.pre) {
        if (!spans[0].elem) spans[0].text = spans[0].text.replace(/^\s+/, "");
        const l = spans[spans.length - 1];
        if (!l.elem) l.text = l.text.replace(/\s+$/, "");
      }
      for (const s of spans) delete s.trail;
      while (spans.length && !spans[0].elem && !/\S/.test(spans[0].text)) spans.shift();
      while (spans.length && !spans[spans.length - 1].elem && !/\S/.test(spans[spans.length - 1].text)) spans.pop();
      const real = spans;
      if (real.length) emit(cur.el, cur.kind, cur.level, real);
      const top = stack[stack.length - 1];
      cur = top ? { spans: [], el: top.el, kind: top.kind, level: top.level, pre: top.pre } : null;
    };
    const emit = (el, kind, level, spans, extra) => {
      const b = { order: order++, frame: 0, kind };
      if (level) b.level = level;
      const lm = landmarkOf(el);
      if (lm) b.landmark = lm.name;
      if (kind === "heading") {
        headings.push({ name: spans.map((s) => s.text).join(""), lmEl: lm ? lm.el : null });
      } else {
        for (let i = headings.length - 1; i >= 0; i--) {
          const h = headings[i];
          if (!h.lmEl || contains(h.lmEl, el)) {
            if (h.name) b.heading = h.name;
            break;
          }
        }
      }
      if (extra) Object.assign(b, extra);
      b.spans = spans.map((s) => (s.elem ? { text: s.text, elem: s.elem } : { text: s.text }));
      b._el = el;
      blocks.push(b);
    };
    const add = (t, elem) => {
      if (!cur) open(doc.body || doc.documentElement, "paragraph", 0, false);
      cur.spans.push(elem ? { text: t, elem } : { text: t });
    };
    const listDepth = () => stack.filter((s) => s.list).length;

    const kids = (n) => {
      if (n.nodeType === 1) {
        if (n.shadowRoot) return n.shadowRoot.childNodes;
        if (n.localName === "slot" && typeof n.assignedNodes === "function") {
          const as = n.assignedNodes();
          if (as.length) return as;
        }
      }
      return n.childNodes;
    };

    const cellSpans = (cell) => {
      // Flatten a cell into one span; keep a link when the cell holds exactly one.
      const parts = [];
      const elems = [];
      const v = (n) => {
        if (n.nodeType === 3) {
          if (!S.consumed.has(n)) parts.push(n.data);
          return;
        }
        if (n.nodeType !== 1) return;
        const rec = S.rec.get(n);
        if (!rec || rec.kind === HIDDEN || rec.challenge || S.consumed.has(n)) return;
        if (attr(n, "aria-hidden") === "true") return;
        if (rec.row && rec.skipKids) {
          const t = rec.row.role === "heading" ? rec.row.name : textOf(n) || rec.row.name;
          if (t) parts.push(" " + t + " ");
          if (rec.row.role !== "heading" && rec.row.role !== "textbox") elems.push("#" + rec.row.id);
          return;
        }
        for (const k of kids(n)) v(k);
        if (isBlock(n, rec)) parts.push(" ");
      };
      for (const k of kids(cell)) v(k);
      const t = clean(parts.join(""), 400);
      return elems.length === 1 ? { text: t, elem: elems[0] } : { text: t };
    };

    const tableRow = (tr, rec) => {
      flush();
      const cells = cellsOf(tr);
      if (!cells.length) return false;
      const spans = cells.map(cellSpans);
      if (rec.row && !spans[0].elem) spans[0].elem = "#" + rec.row.id;
      if (!spans.some((s) => s.text || s.elem)) return true;
      const header = cells.every((c) => c.localName === "th" || ariaRole(c) === "columnheader") ||
        (tr.parentElement && tr.parentElement.localName === "thead");
      const extra = { table: tableId(tr.closest("table")) };
      if (header) extra.header = true;
      emit(tr, "table_row", 0, spans, extra);
      return true;
    };

    const visit = (n) => {
      if (n.nodeType === 3) {
        if (S.consumed.has(n)) return;
        const p = n.parentNode;
        const pr = p && p.nodeType === 1 ? S.rec.get(p) : null;
        if (pr && !pr.vis) return;
        add(n.data);
        return;
      }
      if (n.nodeType !== 1) return;
      const rec = S.rec.get(n);
      if (!rec || rec.kind === HIDDEN || rec.challenge) return;
      if (S.consumed.has(n) || attr(n, "aria-hidden") === "true") return;
      const tag = n.localName;
      const row = rec.row;
      if (tag === "tr" || ariaRole(n) === "row") {
        if (tableRow(n, rec)) return;
      }
      if (row && rec.skipKids) {
        if (row.isHeading) {
          flush();
          const ht = textOf(n) || row.name;
          if (ht) emit(n, "heading", row.level, [{ text: ht }]);
          return;
        }
        if (row.role === "iframe") {
          // Placeholder where a child frame's blocks belong (sb-driver interleaves frames).
          flush();
          emit(n, "other", 0, [{ text: row.name || "frame", elem: "#" + row.id }]);
          return;
        }
        if (FIELD_ROLES.has(row.role) || row.role === "checkbox" || row.role === "radio" || row.role === "switch") return;
        const t = textOf(n) || row.name;
        if (t) add(" " + t + " ", "#" + row.id);
        return;
      }
      if (rec.skipKids) return;
      const block = isBlock(n, rec) || (row && row.isHeading);
      if (block) {
        flush();
        let [kind, level] = row && row.isHeading ? ["heading", row.level] : blockKind(n);
        const isList = tag === "ul" || tag === "ol";
        if (kind === "list_item") level = Math.min(255, Math.max(1, listDepth())); // u8 on the wire
        const pre = kind === "code" || (stack.length && stack[stack.length - 1].pre);
        if (stack.length && stack[stack.length - 1].kind === "code") kind = "code";
        stack.push({ el: n, kind, level, pre, list: isList });
        open(n, kind, level, pre);
        for (const k of kids(n)) visit(k);
        flush();
        stack.pop();
        const top = stack[stack.length - 1];
        cur = top ? { spans: [], el: top.el, kind: top.kind, level: top.level, pre: top.pre } : null;
        return;
      }
      if (tag === "br") {
        add(cur && cur.pre ? "\n" : " ");
        return;
      }
      for (const k of kids(n)) visit(k);
    };
    if (doc.body) visit(doc.body);
    flush();

    let out = blocks;
    if (region !== undefined && region !== null && region !== "") {
      const pred = regionPred(region);
      out = blocks.filter((b) => pred(b));
    }
    for (const b of out) delete b._el;
    return out;
  }

  function regionPred(region) {
    const r = String(region).trim();
    const LMS = new Set(["main", "nav", "header", "footer", "aside", "form", "region", "dialog"]);
    if (LMS.has(r.toLowerCase())) return (b) => b.landmark === r.toLowerCase();
    if (/^f\d+$/.test(r)) {
      const ref = S.formEls.get(r);
      const f = ref && ref.deref();
      return (b) => !!f && contains(f, b._el);
    }
    if (/^#?\d+$/.test(r)) {
      const el = resolve(Number(r.replace("#", "")));
      return (b) => el && el.nodeType === 1 && contains(el, b._el);
    }
    const want = r.toLowerCase();
    return (b) => (b.heading && b.heading.toLowerCase() === want) ||
      (b.kind === "heading" && b.spans.map((s) => s.text).join("").toLowerCase() === want);
  }

  // ---------------------------------------------------------------- act(id) (B-4 #6)
  // One animation frame, capped: rAF never fires in a hidden or background tab, so the cap
  // is the condition's deadline, not a sleep.
  function nextFrame() {
    return new Promise((res) => {
      let done = false;
      const f = () => {
        if (!done) {
          done = true;
          res();
        }
      };
      if (typeof requestAnimationFrame === "function") requestAnimationFrame(f);
      setTimeout(f, 100);
    });
  }
  const sameBox = (a, b) => Math.abs(a.x - b.x) < 0.5 && Math.abs(a.y - b.y) < 0.5 &&
    Math.abs(a.width - b.width) < 0.5 && Math.abs(a.height - b.height) < 0.5;

  function inViewport(el) {
    const r = el.getBoundingClientRect();
    const vp = viewport();
    return r.top >= 0 && r.left >= 0 && r.bottom <= vp.h && r.right <= vp.w;
  }

  async function act(id) {
    const el = resolve(id);
    if (!el || el.why) return el || { why: "gone" };
    if (!el.isConnected) return { why: "gone" };
    if (selfChallenge() || inChallenge(el)) return { why: "challenge" };
    if (!inViewport(el)) {
      if (typeof el.scrollIntoViewIfNeeded === "function") el.scrollIntoViewIfNeeded(true);
      else el.scrollIntoView({ block: "center", inline: "center", behavior: "instant" });
    }
    const cs = style(el);
    const r0 = el.getBoundingClientRect();
    if (r0.width <= 0 || r0.height <= 0 || cs.visibility !== "visible" ||
      (typeof el.checkVisibility === "function" && !el.checkVisibility({ visibilityProperty: true }))) {
      return { why: "hidden" };
    }
    await nextFrame();
    if (!el.isConnected) return { why: "gone" };
    const r1 = el.getBoundingClientRect();
    await nextFrame();
    if (!el.isConnected) return { why: "gone" };
    const r2 = el.getBoundingClientRect();
    if (!sameBox(r1, r2)) return { why: "not stable (moving)" };
    if (r2.width <= 0 || r2.height <= 0) return { why: "hidden" };
    if (isDisabled(el)) return { why: "disabled" };
    const p = centre(el);
    if (!p) return { why: "outside the viewport" };
    const hit = deepHit(p.x, p.y);
    if (!hitsSelf(el, hit)) return { why: coverWhy(el, hit) };
    return { x: Math.round(p.x * 10) / 10, y: Math.round(p.y * 10) / 10 };
  }

  // ---------------------------------------------------------------- settle() (B-4 #8)
  // attributeOldValue: a type or autocomplete that was sensitive keeps the element sensitive.
  const MO_OPTS = { subtree: true, childList: true, attributes: true, characterData: true, attributeOldValue: true };
  const SENS_ATTR = { type: /^\s*password\s*$/i, autocomplete: /\b(current-password|new-password|one-time-code)\b/i };
  const RECENT_MS = 5000;

  function markDirty(node) {
    let el = null;
    if (node.nodeType === 1) el = node;
    else if (node.nodeType === 11 && node.host) el = node.host;
    else if (node.parentNode) {
      const p = node.parentNode;
      el = p.nodeType === 1 ? p : p.host || null;
    }
    if (!el || el === doc.head || (el.closest && el.closest("head")) ||
      el.localName === "style" || el.localName === "link") {
      S.full = true;
      return;
    }
    S.dirty.add(el);
    const lab = el.closest && el.closest("label");
    if (lab && lab.control) S.dirty.add(lab.control);
    if (S.dirty.size > DIRTY_MAX) S.full = true;
  }

  // Recently changed nodes for settle's animation check: [t, WeakRef<el>, self]. self: an
  // attribute or text change, which only counts the node's own animations (a class toggle on
  // <html> must not let every animation in the page hold settle).
  function remember(t, n, self) {
    if (!n) return;
    const el = n.nodeType === 1 ? n : n.parentElement;
    if (!el) return;
    S.recent.push([t, new WeakRef(el), self]);
    if (S.recent.length > 300) S.recent.splice(0, S.recent.length - 300);
  }

  // Fields that hold a value (D-438 (2)): textarea, and inputs other than these types. Only they
  // count in the one-for-one swap and in the add-side memory, so a show-password checkbox or a
  // hidden input next to the password does not break a wrapper swap.
  const NONVALUE_TYPES = new Set(["checkbox", "radio", "hidden", "button", "submit", "reset", "image", "file", "range", "color"]);
  const valueField = (f) => f.localName === "textarea" || !NONVALUE_TYPES.has(String(f.type).toLowerCase());
  // Value fields in a node (itself or its light-DOM subtree).
  function fieldsIn(n) {
    if (!n || n.nodeType !== 1) return [];
    const own = n.localName === "input" || n.localName === "textarea" ? [n] : [];
    const all = n.firstElementChild ? own.concat(Array.from(n.querySelectorAll("input,textarea"))) : own;
    return all.filter(valueField);
  }

  // A show-password toggle that rebuilds the element (replaceWith, a re-render) hands the value
  // to a new node. The new field inherits sensitivity only when it matches a removed sensitive
  // field (D-435 (1)): a one-for-one swap (in one batch, one parent lost exactly one value field,
  // a sensitive one, and gained exactly one, counting subtrees: replaceWith of the input or of
  // its wrapper, remove + append, D-437), or a shared id, name, autocomplete token, aria-label,
  // placeholder or label text. A re-rendered page whose other fields match nothing (an SPA login
  // swap removes at least two fields) keeps reading their values. Removed sensitive fields are
  // remembered (signature and removal time, never a value) for SENS_KEEP_MS, so a rebuild split
  // by a microtask or a later task still matches (D-435 (2)); entries age out on use.
  // Add-first rebuilds (D-438 (1); Vue <Transition>, framer-motion AnimatePresence, Svelte
  // outros insert the new field and remove the old one later): an added field is checked at
  // once against the sensitive fields already in its parent (the overlap read), and every added
  // value field that stays non-sensitive is remembered (WeakRef and add time) for the same
  // window, so a later removal of a sensitive field marks the ones that match its keys, or the
  // single one added to the same parent. Remove-first across batches (D-439, Vue out-in, a
  // re-render split by an await): a parent that lost exactly one value field, a sensitive one,
  // and gained none is remembered (WeakRef) with the removal; the first later batch within the
  // window that adds value fields to it marks its field when it adds exactly one.
  const SENS_KEEP_MS = 2000;
  const SENS_KEEP_MAX = 32;
  const BORN_MAX = 64;
  const NEAR_MAX = 64; // fields per parent whose sensitivity is computed for the overlap check
  const KEY_CAP = 120;
  // Autocomplete tokens every kind of field shares: never a match on their own.
  const AC_GENERIC = new Set(["on", "off", "shipping", "billing", "home", "work", "mobile", "fax", "pager", "webauthn"]);

  const foldKey = (v) => String(v || "").replace(/\s+/g, " ").trim().replace(/[\s:*]+$/, "").toLowerCase().slice(0, KEY_CAP);
  // Detached roots to search besides the document: the removed nodes of one record.
  const detached = (roots) => (roots || []).filter((r) => r && r.nodeType === 1 && !r.isConnected);
  // An element with this id: in the document, else inside one of the detached roots.
  function byIdIn(id, roots) {
    const d = doc.getElementById(id);
    if (d) return d;
    for (const root of detached(roots)) {
      if (root.id === id) return root;
      for (const e of root.querySelectorAll("[id]")) if (e.id === id) return e;
    }
    return null;
  }
  // Labels of a field: el.labels when attached, else the wrapping label and label[for] in the
  // document or in the nodes removed by the same record (a label removed as a sibling of the
  // field counts, D-438 (3)).
  function fieldLabels(f, roots) {
    const out = new Set();
    try {
      if (f.isConnected && f.labels) {
        for (const l of f.labels) out.add(l);
        return out;
      }
    } catch (e) { /* no labels API */ }
    const wrap = f.parentNode && f.parentNode.closest ? f.parentNode.closest("label") : null;
    if (wrap) out.add(wrap);
    if (f.id) {
      for (const l of doc.querySelectorAll("label[for]")) if (l.htmlFor === f.id) out.add(l);
      for (const root of detached(roots)) {
        if (root.localName === "label" && root.htmlFor === f.id) out.add(root);
        for (const l of root.querySelectorAll("label[for]")) if (l.htmlFor === f.id) out.add(l);
      }
    }
    return out;
  }
  // Identity keys of a field. Attributes and label text only: textOf skips field contents,
  // so a value (or a textarea's default text) never enters a key.
  function fieldKeys(f, roots) {
    const keys = new Set();
    const add = (k, v) => {
      const x = foldKey(v);
      if (x) keys.add(k + x);
    };
    add("id:", f.id);
    add("name:", attr(f, "name"));
    for (const tok of attr(f, "autocomplete").toLowerCase().split(/\s+/)) {
      if (tok && !AC_GENERIC.has(tok) && !tok.startsWith("section-")) add("ac:", tok);
    }
    add("t:", attr(f, "aria-label"));
    add("t:", attr(f, "placeholder"));
    for (const l of fieldLabels(f, roots)) add("t:", textOf(l, f));
    for (const ref of attr(f, "aria-labelledby").split(/\s+/)) {
      const e = ref ? byIdIn(ref, roots) : null;
      if (e && e !== f) add("t:", textOf(e, f));
    }
    const r = S.rec.get(f);
    if (r && r.row && r.row.el === f && r.row.name) add("t:", r.row.name);
    return keys;
  }
  // keys: an array, or null for a field whose identity could not be read (matches everything).
  const keysHit = (keys, set) => keys === null || keys.some((k) => set.has(k));
  function markSens(f) {
    S.sens.add(f);
    if (S.walked && !S.full) markDirty(f);
  }

  function inheritSensitive(recs) {
    const t = now();
    const live = (x) => t - x.t <= SENS_KEEP_MS;
    if (S.gone.length && !live(S.gone[0])) S.gone = S.gone.filter(live);
    if (S.born.length && !live(S.born[0])) S.born = S.born.filter(live);
    // parent -> { out: removed fields, sens: sensitive among them, add: added fields, gone: the
    // S.gone entry of its sensitive field, swap: its one-for-one swap applied (D-437, D-440) }
    let swap = null;
    const lost = []; // [{ keys, parent, e }] sensitive fields this batch removed (e: swap entry)
    const gone = []; // S.gone entries of this batch
    for (const m of recs) {
      if (m.type !== "childList" || !m.removedNodes.length) continue;
      const roots = Array.from(m.removedNodes);
      for (const r of roots) {
        for (const f of fieldsIn(r)) {
          const e = (swap || (swap = new Map())).get(m.target) || { out: 0, sens: 0, add: null, gone: null, swap: false };
          swap.set(m.target, e);
          e.out++;
          let s = true; // sensitivity unreadable (a throwing getter): sensitive, fail closed
          try {
            s = sensitive(f);
          } catch (err) { /* s stays true */ }
          if (!s) continue;
          e.sens++;
          let keys = null; // identity unreadable: remember a match-all entry (D-438 (3), fail closed)
          try {
            keys = Array.from(fieldKeys(f, roots));
          } catch (err) { /* keys stays null */ }
          e.gone = { t, keys, parent: null };
          gone.push(e.gone);
          lost.push({ keys, parent: m.target, e });
        }
      }
    }
    if (swap) {
      for (const m of recs) {
        const e = m.type === "childList" && m.addedNodes.length ? swap.get(m.target) : null;
        if (!e) continue;
        for (const a of m.addedNodes) for (const f of fieldsIn(a)) (e.add || (e.add = [])).push(f);
      }
      for (const [p, e] of swap) {
        if (e.out !== 1 || e.sens !== 1) continue;
        // one out, one in within this batch (D-437); one out and nothing in: a later batch that
        // adds exactly one value field to this parent within the window inherits (D-439)
        if (!e.add) e.gone.parent = new WeakRef(p);
        else if (e.add.length === 1) {
          S.sens.add(e.add[0]);
          e.swap = true;
        }
      }
    }
    for (const g of gone) if (g.keys === null || g.keys.length || g.parent) S.gone.push(g);
    if (S.gone.length > SENS_KEEP_MAX) S.gone.splice(0, S.gone.length - SENS_KEEP_MAX);
    // Add-first: fields added by earlier batches, still in the page, matched against the
    // sensitive fields this batch removed.
    if (lost.length && S.born.length) {
      const prior = [];
      for (const b of S.born) {
        const f = b.ref.deref();
        if (f && f.isConnected && !S.sens.has(f)) prior.push({ f, p: b.parent.deref(), keys: undefined });
      }
      for (const g of lost) {
        // D-440: where this batch's one-for-one swap (D-437) already handed sensitivity to a new
        // field of the parent, the single field added to it earlier (a parse-time neighbour) does
        // not inherit. Key matches still do.
        const inParent = g.e.swap ? [] : prior.filter((x) => x.p === g.parent);
        for (const x of prior) {
          if (S.sens.has(x.f)) continue;
          let hit = g.keys === null || (inParent.length === 1 && inParent[0] === x);
          if (!hit) {
            try {
              if (x.keys === undefined) x.keys = fieldKeys(x.f, null);
              hit = keysHit(g.keys, x.keys);
            } catch (e) {
              hit = true; // fail closed
            }
          }
          if (hit) markSens(x.f);
        }
      }
      S.born = S.born.filter((b) => {
        const f = b.ref.deref();
        return f && !S.sens.has(f);
      });
    }
    // Fields this batch added: match the remembered removals and the sensitive fields still
    // next to them (same parent, not added by this batch); remember the rest.
    const added = [];
    for (const m of recs) {
      if (m.type !== "childList" || !m.addedNodes.length) continue;
      for (const a of m.addedNodes) for (const f of fieldsIn(a)) added.push([f, m.target]);
    }
    if (!added.length) return;
    // Remove-first (D-439): parents an earlier batch left with no value field after removing
    // their only one, a sensitive one. The first later batch that adds value fields to such a
    // parent consumes the entry; its field inherits when it is the only one.
    const count = new Map(); // parent -> value fields this batch added to it
    for (const [, p] of added) count.set(p, (count.get(p) || 0) + 1);
    let heir = null; // parents whose single added field inherits
    for (const g of S.gone) {
      const gp = g.parent ? g.parent.deref() : null;
      if (!gp || !count.has(gp)) continue;
      g.parent = null;
      if (count.get(gp) === 1) (heir || (heir = new Set())).add(gp);
    }
    const fresh = new Set(added.map((x) => x[0]));
    const near = new Map(); // parent -> [{ s, keys }] sensitive fields already in it
    const nearOf = (p) => {
      let v = near.get(p);
      if (v) return v;
      v = [];
      if (p && typeof p.querySelectorAll === "function") {
        let n = 0;
        for (const s of p.querySelectorAll("input,textarea")) {
          if (fresh.has(s) || !valueField(s)) continue;
          if (S.sens.has(s) || (n++ < NEAR_MAX && sensitive(s))) v.push({ s, keys: undefined });
        }
      }
      near.set(p, v);
      return v;
    };
    for (const [f, p] of added) {
      let hit = false;
      // every read of the added field and its neighbours runs inside the try: a getter that
      // throws marks the field sensitive and leaves the rest of the batch to run (D-439)
      try {
        if (S.sens.has(f) || sensitive(f)) continue;
        hit = !!heir && heir.has(p);
        const sib = hit ? [] : nearOf(p);
        if (!hit && (S.gone.length || sib.length)) {
          const keys = fieldKeys(f, null);
          hit = S.gone.some((g) => keysHit(g.keys, keys));
          for (const x of sib) {
            if (hit) break;
            if (x.keys === undefined) x.keys = Array.from(fieldKeys(x.s, null));
            hit = keysHit(x.keys, keys);
          }
        }
      } catch (e) {
        hit = true; // a field whose sensitivity or keys cannot be read: sensitive (fail closed)
      }
      if (hit) S.sens.add(f);
      else S.born.push({ t, ref: new WeakRef(f), parent: new WeakRef(p) });
    }
    if (S.born.length > BORN_MAX) S.born.splice(0, S.born.length - BORN_MAX);
  }

  function flushMut() {
    const recs = mo.takeRecords();
    if (recs.length) onMut(recs);
  }

  function onMut(recs) {
    const t = now();
    S.lastMut = t;
    S.mutCount += recs.length;
    for (const m of recs) {
      if (m.type === "attributes" && m.oldValue && SENS_ATTR[m.attributeName] &&
        SENS_ATTR[m.attributeName].test(m.oldValue) && m.target.nodeType === 1) {
        S.sens.add(m.target);
      }
    }
    inheritSensitive(recs);
    for (const m of recs) {
      if (S.walked && !S.full) markDirty(m.target);
      if (m.type === "childList") for (const a of m.addedNodes) remember(t, a, false);
      else remember(t, m.target, true);
    }
    for (const w of Array.from(S.waiters)) w();
  }

  const mo = new MutationObserver(onMut);
  mo.observe(doc, MO_OPTS);
  doc.addEventListener("readystatechange", () => {
    for (const w of Array.from(S.waiters)) w();
  });
  // Each edit records the field's empty|filled evidence (caret, validity; never the value),
  // so a later type swap that resets the caret still reports it (D-426).
  doc.addEventListener("input", (e) => {
    const t = typeof e.composedPath === "function" ? e.composedPath()[0] : e.target;
    if (!t || t.nodeType !== 1 || (t.localName !== "input" && t.localName !== "textarea")) return;
    const ev = fillEvidence(t);
    if (ev) S.lastFill.set(t, { ev, type: String(t.type).toLowerCase() });
    else S.lastFill.delete(t);
  }, true);

  function infinite(a) {
    try {
      const tm = a.effect.getComputedTiming();
      return tm.endTime === Infinity || tm.iterations === Infinity;
    } catch (e) {
      return false;
    }
  }

  function runningAnims(since) {
    if (typeof doc.getAnimations !== "function") return [];
    const cut = now() - RECENT_MS;
    if (S.recent.length && S.recent[0][0] < cut) S.recent = S.recent.filter((r) => r[0] >= cut);
    const changed = [];
    for (const [t, ref, self] of S.recent) {
      const el = t >= since ? ref.deref() : null;
      if (el && el.isConnected) changed.push({ el, self: self || el === doc.body || el === doc.documentElement });
    }
    if (!changed.length) return [];
    return doc.getAnimations().filter((a) => {
      if (a.playState !== "running" || !a.effect || !a.effect.target) return false;
      // An infinite animation (spinner, shimmer, marquee) never ends: it cannot hold settle.
      if (infinite(a)) return false;
      const tg = a.effect.target;
      return changed.some((c) => (c.self ? c.el === tg : contains(c.el, tg)));
    });
  }

  // Something a reader can use: a control, media, an embed (PDF), or a visible text node.
  function hasContent() {
    const b = doc.body;
    if (!b) return false;
    if (b.querySelector("input:not([type=hidden]),button,select,textarea,img,iframe,video,canvas,svg,a[href],embed,object")) {
      return true;
    }
    const tw = doc.createTreeWalker(b, NodeFilter.SHOW_TEXT);
    let t;
    while ((t = tw.nextNode())) {
      if (/\S/.test(t.data) && t.parentNode && !SKIP_TAGS.has(t.parentNode.localName)) return true;
    }
    let i = 0;
    for (const el of b.querySelectorAll("*")) {
      if (i++ > 500) break;
      if (el.shadowRoot && el.shadowRoot.childElementCount) return true;
    }
    return false;
  }

  // The D-416 content wait applies only to a document that may still render itself: it has
  // script elements, is not about:blank / about:srcdoc, and its body has no content yet
  // (D-405 (8)). A blank tab, a PDF embed or a static page never waits for content.
  function wantsContent() {
    try {
      if (String(globalThis.location && globalThis.location.protocol) === "about:") return false;
    } catch (e) { /* no location */ }
    return !!doc.querySelector("script") && !hasContent();
  }

  // Resolves when the DOM has had no mutation for `quiet` ms (counted from the call), no
  // running finite animation on recently changed nodes, the document is past loading and,
  // for a scripted page with an empty body, has content or `content_ms` (default 1000) has
  // passed since the call (D-416, D-405 (8)); or when `cap` ms pass. Every wait below is on
  // a condition with that cap.
  function settle(opts) {
    opts = opts || {};
    const quiet = Number(opts.quiet ?? opts.quiet_ms ?? QUIET_MS);
    const cap = Number(opts.cap ?? opts.cap_ms ?? CAP_MS);
    const contentMs = Number(opts.content_ms ?? opts.contentMs ?? CONTENT_MS);
    const t0 = now();
    const c0 = S.mutCount;
    return new Promise((resolveP) => {
      let done = false;
      let timer = 0;
      let animWait = false;
      const finish = (settled) => {
        if (done) return;
        done = true;
        clearTimeout(timer);
        clearTimeout(capTimer);
        S.waiters.delete(wake);
        resolveP({ settled, changed: S.mutCount - c0 });
      };
      // The cap is the deadline of the condition below.
      const capTimer = setTimeout(() => finish(false), Math.max(0, cap));
      const arm = () => {
        if (done || animWait) return;
        clearTimeout(timer);
        const due = Math.max(S.lastMut, t0) + quiet - now();
        // Timer to the instant the quiet condition can first hold, re-armed on each mutation.
        timer = setTimeout(check, Math.max(0, due));
      };
      const wake = () => arm();
      const check = () => {
        timer = 0;
        if (done) return;
        if (now() - t0 >= cap) return finish(false);
        if (doc.readyState === "loading") return; // readystatechange wakes us
        if (now() - Math.max(S.lastMut, t0) < quiet) return arm();
        const anims = runningAnims(t0 - 1000);
        if (anims.length) {
          animWait = true;
          Promise.race(anims.map((a) => a.finished.catch(() => undefined))).then(() => {
            animWait = false;
            if (!done) check();
          });
          return;
        }
        const left = t0 + contentMs - now();
        if (left > 0 && wantsContent()) {
          // The next mutation wakes us; this timer is the content condition's own deadline.
          timer = setTimeout(check, left);
          return;
        }
        finish(true);
      };
      S.waiters.add(wake);
      arm();
    });
  }

  // ---------------------------------------------------------------- fingerprint (T-4)
  function parseSpec(spec) {
    const items = [];
    const s = String(spec || "");
    let i = 0;
    while (i < s.length) {
      while (i < s.length && /\s/.test(s[i])) i++;
      if (i >= s.length) break;
      const start = i;
      let role = "";
      while (i < s.length && /[A-Za-z0-9_-]/.test(s[i])) role += s[i++];
      let id = null;
      if (s[i] === "#") {
        i++;
        id = "";
        while (i < s.length && !/[\s"]/.test(s[i])) id += s[i++];
      }
      let name = null;
      if (s[i] === '"') {
        i++;
        name = "";
        while (i < s.length && s[i] !== '"') {
          if (s[i] === "\\" && i + 1 < s.length) i++;
          name += s[i++];
        }
        i++;
      }
      if (start === i) {
        i++;
        continue;
      }
      const raw = s.slice(start, i);
      if (role && /^[A-Z]+$/.test(role) && id === null && name === null && items.length) {
        items[items.length - 1].flags.push(role.toLowerCase());
        items[items.length - 1].raw += " " + raw;
        continue;
      }
      items.push({ role: role.toLowerCase(), id, name, flags: [], raw });
    }
    return items;
  }

  function nameMatch(have, want) {
    if (!have) return false;
    const h = clean(have).toLowerCase();
    const w = clean(want).toLowerCase();
    return h === w || h.includes(w);
  }

  function fingerprint(spec) {
    if (!S.based) return NO_BASE();
    const items = parseSpec(spec);
    if (!items.length) return "match";
    const rows = index();
    const missing = [];
    for (const it of items) {
      let ok;
      if (it.role === "form") {
        if (it.id) {
          const ref = S.formEls.get(it.id);
          const f = ref && ref.deref();
          ok = !!f && f.isConnected;
        } else {
          ok = rows.some((r) => r.form);
        }
      } else {
        const roles = it.role ? ALIASES[it.role] || [it.role] : null;
        ok = rows.some((r) => {
          if (roles && !roles.includes(r.role)) return false;
          if (it.id) {
            if (/^\d+$/.test(it.id)) {
              if (r.id !== Number(it.id)) return false;
            } else {
              const el = resolve(r.id);
              if (!el || el.why || el.id !== it.id) return false;
            }
          }
          if (it.name !== null && !nameMatch(r.name, it.name)) return false;
          for (const f of it.flags) if (!r.flags[f]) return false;
          return true;
        });
      }
      if (!ok) missing.push(it.raw);
    }
    if (!missing.length) return "match";
    if (missing.length === items.length) return "miss";
    return { partial: missing };
  }

  // ---------------------------------------------------------------- frames()
  function frames() {
    if (!S.based) return NO_BASE();
    ensure();
    const out = [{
      frame: 0, parent: null, url: String(globalThis.location && globalThis.location.href),
      opaque: false, challenge: S.selfCh,
    }];
    for (const rec of S.rows) {
      const row = rec.row;
      if (row.role !== "iframe" || !row.el.isConnected) continue;
      const el = row.el;
      let opaque = false;
      try {
        opaque = el.contentDocument == null;
      } catch (e) {
        opaque = true;
      }
      const src = attr(el, "src");
      let url = el.hasAttribute("srcdoc") ? "about:srcdoc" : "about:blank";
      if (src) {
        try {
          url = new URL(src, doc.baseURI).href;
        } catch (e) {
          url = src;
        }
      }
      out.push({ frame: null, id: row.id, parent: 0, url, opaque, challenge: row.challenge, name: row.name });
    }
    return out;
  }

  // ---------------------------------------------------------------- single-element API
  function rowFor(el) {
    if (!el || el.nodeType !== 1) return null;
    ensure();
    if (S.selfCh) return null; // never read inside a challenge page
    let rec = S.rec.get(el);
    if (!rec || !rec.row) {
      S.layout = hasLayout();
      S.fresh = [];
      rec = compute(el);
      for (const r of S.fresh) finishRow(r);
      S.fresh = [];
      if (!rec.row) return null;
    }
    return rec.row;
  }
  function nameApi(el) {
    if (!S.based) return NO_BASE();
    const row = rowFor(el);
    if (row) return row.name;
    return el && el.nodeType === 1 && !S.selfCh ? ladder(el, roleOf(el)) : "";
  }
  function roleApi(el) {
    return el && el.nodeType === 1 ? roleOf(el) : "";
  }
  function stateApi(el) {
    if (!S.based) return NO_BASE();
    const row = rowFor(el);
    if (!row) return null;
    const id = row.id;
    const r = index().find((o) => o.id === id);
    return r ? r.state : null;
  }

  const api = {
    walk,
    id: idApi,
    resolve,
    name: nameApi,
    role: roleApi,
    state: stateApi,
    index,
    text,
    act,
    settle,
    fingerprint,
    frames,
  };
  Object.defineProperty(globalThis, "__sb", {
    value: Object.freeze(api),
    configurable: false,
    enumerable: false,
    writable: false,
  });
})();
