// Injected into the active browser tab (web.rs). One function: P = {op, id?, value?} -> a JSON string.
// scan: the visible actionable elements, leaves before containers (vc-feedback 24). Each gets a stable
// data-susurro-id, so a later act finds the same element. act ops: click, focus, fill, scroll, sig.
(function (P) {
  try {
    var ATTR = "data-susurro-id";
    var MAX = 150;
    var SEL = 'a[href],button,input,textarea,select,summary,[role=button],[role=link],[role=tab],[role=menuitem],[role=checkbox],[role=switch],[role=radio],[role=option],[role=combobox],[role=searchbox],[role=textbox],[onclick],[contenteditable=""],[contenteditable=true],[data-color],.swatch,[class*=color-button]';
    var NATIVE = { A: 1, BUTTON: 1, INPUT: 1, TEXTAREA: 1, SELECT: 1, SUMMARY: 1 };
    var clip = function (s) {
      s = (s || "").replace(/\s+/g, " ").trim();
      return s.length > 60 ? s.slice(0, 59) + "…" : s;
    };
    // every match, open shadow roots included (Reddit, Salesforce: web components)
    var deep = function (sel) {
      var out = [];
      var walk = function (root) {
        out.push.apply(out, root.querySelectorAll(sel));
        var all = root.querySelectorAll("*");
        for (var i = 0; i < all.length; i++) if (all[i].shadowRoot) walk(all[i].shadowRoot);
      };
      walk(document);
      return out;
    };
    var byId = function (id) {
      return /^[0-9]+$/.test(String(id)) ? deep("[" + ATTR + '="' + id + '"]')[0] || null : null;
    };
    var secret = function (e) {
      return e.tagName === "INPUT" && (e.type === "password" || e.autocomplete === "current-password" || e.autocomplete === "new-password");
    };
    var roleOf = function (e) {
      var r = e.getAttribute("role");
      var t = e.tagName;
      if (r === "checkbox") return "checkbox";
      if (r === "switch") return "switch";
      if (r === "radio") return "radio button";
      if (r === "tab") return "tab";
      if (r === "menuitem" || r === "option") return "menu item";
      if (r === "combobox") return "combo box";
      if (r === "searchbox") return "search field";
      if (r === "textbox") return "text field";
      if (r === "link") return "link";
      if (r === "button") return "button";
      if (t === "A") return "link";
      if (t === "TEXTAREA") return "text area";
      if (t === "SELECT") return "pop-up button";
      if (t === "SUMMARY") return "disclosure";
      if (t === "INPUT") {
        var ty = (e.type || "text").toLowerCase();
        if (ty === "checkbox") return "checkbox";
        if (ty === "radio") return "radio button";
        if (ty === "search") return "search field";
        if (ty === "range") return "slider";
        if (/^(button|submit|reset|image|file|color)$/.test(ty)) return "button";
        return "text field";
      }
      if (e.isContentEditable) return "text area";
      return "button";
    };
    var takesText = function (role) {
      return role === "text field" || role === "text area" || role === "combo box" || role === "search field";
    };
    var nameOf = function (e) {
      var a = e.getAttribute("aria-label");
      if (a && a.trim()) return clip(a);
      var lb = e.getAttribute("aria-labelledby");
      if (lb) {
        var t = lb.split(/\s+/).map(function (i) { var x = document.getElementById(i); return x ? x.textContent : ""; }).join(" ");
        if (t.trim()) return clip(t);
      }
      if (e.labels && e.labels.length) {
        var l = e.labels[0].innerText;
        if (l && l.trim()) return clip(l);
      }
      if (e.tagName === "INPUT" || e.tagName === "TEXTAREA") {
        var ph = e.placeholder || e.title || ((/^(button|submit|reset)$/i.test(e.type)) ? e.value : "") || e.name;
        return clip(ph);
      }
      var tx = e.innerText;
      if (tx && tx.trim()) return clip(tx);
      var img = e.querySelector("img[alt],svg[aria-label],[aria-label]");
      var alt = img && (img.getAttribute("alt") || img.getAttribute("aria-label"));
      return clip(alt || e.title || "");
    };
    // A paint palette swatch (JS Paint: div.swatch.color-button[data-color], no text): named by its color, "red".
    // ponytail: the nearest of a few named colors; add a name here when a swatch reads wrong.
    var COLORS = [["black", 0, 0, 0], ["white", 255, 255, 255], ["gray", 128, 128, 128], ["silver", 192, 192, 192], ["red", 255, 0, 0],
      ["dark red", 128, 0, 0], ["yellow", 255, 255, 0], ["olive", 128, 128, 0], ["green", 0, 255, 0], ["dark green", 0, 128, 0],
      ["cyan", 0, 255, 255], ["teal", 0, 128, 128], ["blue", 0, 0, 255], ["navy", 0, 0, 128], ["magenta", 255, 0, 255],
      ["purple", 128, 0, 128], ["orange", 255, 128, 64], ["brown", 128, 64, 0], ["pink", 255, 0, 128], ["light blue", 128, 128, 255]];
    var colorName = function (e) {
      var c = e.getAttribute("data-color") || getComputedStyle(e).backgroundColor || "";
      var m = c.match(/^#([0-9a-f]{2})([0-9a-f]{2})([0-9a-f]{2})/i);
      var v = m ? [parseInt(m[1], 16), parseInt(m[2], 16), parseInt(m[3], 16)] : (c.match(/\d+(\.\d+)?/g) || []).slice(0, 3).map(Number);
      if (v.length < 3 || /^rgba\(.*,\s*0\)$/.test(c)) return "";
      var best = COLORS.reduce(function (b, n) {
        var d = Math.pow(n[1] - v[0], 2) + Math.pow(n[2] - v[1], 2) + Math.pow(n[3] - v[2], 2);
        return d < b[1] ? [n[0], d] : b;
      }, ["", Infinity]);
      return best[0] + " color";
    };
    var swatch = function (e) {
      return e.hasAttribute("data-color") || /(^|\s)(swatch|color-button)(\s|$)/.test(e.className || "");
    };
    var markName = function (e) {
      var nm = nameOf(e);
      return !nm && swatch(e) ? colorName(e) : nm;
    };
    var stateOf = function (e) {
      if (e.tagName === "INPUT" && (e.type === "checkbox" || e.type === "radio")) return e.checked ? "on" : "off";
      var c = e.getAttribute("aria-checked") || e.getAttribute("aria-pressed");
      if (c === "true") return "on";
      if (c === "false") return "off";
      return null;
    };
    var shown = function (e, r) {
      if (r.width < 4 || r.height < 4) return false;
      if (r.bottom <= 0 || r.right <= 0 || r.top >= innerHeight || r.left >= innerWidth) return false;
      var s = getComputedStyle(e);
      if (s.visibility === "hidden" || s.display === "none" || Number(s.opacity) === 0) return false;
      // topmost at its (viewport-clipped) center: not under a modal or a cookie banner
      var x = (Math.max(r.left, 0) + Math.min(r.right, innerWidth)) / 2;
      var y = (Math.max(r.top, 0) + Math.min(r.bottom, innerHeight)) / 2;
      var root = e.getRootNode();
      var hit = (root.elementFromPoint ? root : document).elementFromPoint(x, y);
      return !!hit && (e === hit || e.contains(hit) || hit.contains(e));
    };
    // Changes when a click did something (vc-feedback 21): page, title, the DOM size, the text, every toggle and busy
    // state, a navigation that started (the click op's beforeunload flag). Not the focus: a click focuses its target.
    var sig = function () {
      var n = function (s) { return document.querySelectorAll(s).length; };
      return [location.href, document.title, document.getElementsByTagName("*").length, document.body ? document.body.innerText.length : 0,
        n(":checked"), n(":disabled"), n("[aria-expanded=true]"), n("[aria-selected=true]"), n("[aria-checked=true]"), n("[aria-pressed=true]"),
        n("[aria-busy=true]"), document.documentElement.getAttribute(ATTR + "-nav") || ""].join("|");
    };
    var meta = function () {
      return { url: location.href, title: document.title, iw: innerWidth, ih: innerHeight, sx: screenX, sy: screenY, ow: outerWidth, oh: outerHeight, dpr: devicePixelRatio };
    };

    if (P.op === "scan") {
      var next = Number(document.documentElement.getAttribute(ATTR + "-next") || "1");
      var cands = [];
      var all = deep(SEL);
      for (var i = 0; i < all.length; i++) {
        var e = all[i];
        if (secret(e) || e.disabled || e.getAttribute("aria-disabled") === "true" || (e.tagName === "INPUT" && e.type === "hidden")) continue;
        var r = e.getBoundingClientRect();
        if (!shown(e, r)) continue;
        cands.push({ e: e, r: r, role: roleOf(e), name: markName(e) });
      }
      // Leaves, not containers: drop a candidate that wraps another one with the same name, and a non-native
      // wrapper (onclick div, role=button card) around a real link / button / field.
      var keep = cands.filter(function (c) {
        return !cands.some(function (d) {
          return d !== c && c.e.contains(d.e) && (d.name === c.name || (!NATIVE[c.e.tagName] && NATIVE[d.e.tagName]));
        });
      });
      keep.sort(function (a, b) { return Math.round(a.r.top / 4) - Math.round(b.r.top / 4) || a.r.left - b.r.left; });
      var els = keep.slice(0, MAX).map(function (c) {
        var id = c.e.getAttribute(ATTR);
        if (!id) {
          id = String(next++);
          c.e.setAttribute(ATTR, id);
        }
        var v = takesText(c.role) && !c.e.isContentEditable ? String(c.e.value || "") : c.e.isContentEditable ? c.e.innerText : "";
        return {
          id: Number(id), role: c.role, name: c.name, href: c.e.tagName === "A" ? c.e.href : "", value: v.slice(0, 200), state: stateOf(c.e),
          x: c.r.left, y: c.r.top, w: c.r.width, h: c.r.height,
        };
      });
      document.documentElement.setAttribute(ATTR + "-next", String(next));
      var m = meta();
      m.els = els;
      // a big iframe (payment form, embedded app): its content is not in this DOM, the AX tree reads it (web.rs)
      m.frames = Array.prototype.filter.call(document.querySelectorAll("iframe,frame"), function (f) {
        var r = f.getBoundingClientRect();
        var w = Math.min(r.right, innerWidth) - Math.max(r.left, 0), h = Math.min(r.bottom, innerHeight) - Math.max(r.top, 0);
        return w > 0 && h > 0 && w * h >= 0.25 * innerWidth * innerHeight && shown(f, r);
      }).length;
      return JSON.stringify(m);
    }

    var el = P.id === undefined ? null : byId(P.id);
    if (P.op === "sig") return JSON.stringify({ sig: sig(), url: location.href, title: document.title });
    if (!el) return JSON.stringify({ err: "gone" });
    // the page re-rendered since the scan: the node with this id now shows something else (a reused list row)
    if (typeof P.name === "string" && markName(el) !== P.name) return JSON.stringify({ err: "gone" });
    if (P.op !== "fill") el.scrollIntoView({ block: P.op === "scroll" ? "center" : "nearest", inline: "nearest" });
    if (P.op === "click") {
      addEventListener("beforeunload", function () { document.documentElement.setAttribute(ATTR + "-nav", String(Date.now())); }, { once: true });
      var o = { bubbles: true, cancelable: true, view: window, button: 0 };
      var ptr = typeof PointerEvent === "function" ? PointerEvent : MouseEvent;
      el.dispatchEvent(new ptr("pointerdown", o));
      el.dispatchEvent(new MouseEvent("mousedown", o));
      if (typeof el.focus === "function") el.focus({ preventScroll: true });
      el.dispatchEvent(new ptr("pointerup", o));
      el.dispatchEvent(new MouseEvent("mouseup", o));
      el.click();
    } else if (P.op === "focus") {
      el.focus();
    } else if (P.op === "fill") {
      if (secret(el)) return JSON.stringify({ err: "secure" });
      el.focus();
      if (el.isContentEditable) {
        el.textContent = String(P.value);
      } else {
        // the prototype setter: React / Vue track the value through it
        var proto = el.tagName === "TEXTAREA" ? HTMLTextAreaElement.prototype : el.tagName === "SELECT" ? HTMLSelectElement.prototype : HTMLInputElement.prototype;
        Object.getOwnPropertyDescriptor(proto, "value").set.call(el, String(P.value));
      }
      el.dispatchEvent(new Event("input", { bubbles: true }));
      el.dispatchEvent(new Event("change", { bubbles: true }));
    } else if (P.op !== "scroll") {
      return JSON.stringify({ err: "op" });
    }
    var r2 = el.getBoundingClientRect();
    var res = meta();
    res.focused = document.activeElement === el;
    // false = the browser's own UI (the address bar) has the keys: the JS focus did not reach the keyboard
    res.hasFocus = document.hasFocus();
    res.rect = { x: r2.left, y: r2.top, w: r2.width, h: r2.height };
    return JSON.stringify(res);
  } catch (err) {
    return JSON.stringify({ err: String(err) });
  }
})
