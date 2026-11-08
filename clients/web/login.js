(function () {
  if (customElements.get("dottie-login")) return

  var style = [
    ":host {",
    "  display: flex;",
    "  flex-direction: column;",
    "  justify-content: center;",
    "  align-items: center;",
    "  box-sizing: border-box;",
    "  height: 100dvh;",
    "  padding: 12px;",
    "  gap: 8px;",
    "  background: #0c0500;",
    "  color: #f3b34b;",
    "  font-family: \"JetBrainsMono Nerd Font\", \"JetBrainsMono NF\", ui-monospace, monospace;",
    "  font-weight: 700;",
    "  font-size: 16px;",
    "  line-height: 1.35;",
    "}",
    ":host([hidden]) { display: none !important; }",
    "*, *::before, *::after { box-sizing: border-box; }",
    "button, input { font: inherit; color: inherit; }",
    "button { background: none; border: 0; padding: 0; cursor: pointer; }",
    ".name, .form { width: min(520px, 100%); }",
    ".name { display: flex; justify-content: center; }",
    ".chip {",
    "  display: inline-flex;",
    "  align-items: center;",
    "  justify-content: center;",
    "  height: 36px;",
    "  padding: 0 10px;",
    "  border: 1px solid transparent;",
    "  background: transparent;",
    "  color: #f3b34b;",
    "  font-size: 16px;",
    "  text-align: center;",
    "}",
    ".chip.selected { background: rgb(243 179 75 / 0.18); }",
    ".chip.bordered { border-color: rgb(243 179 75 / 0.4); width: 100%; }",
    ".chip:disabled { cursor: default; opacity: 1; }",
    ".form { display: flex; flex-direction: column; align-items: stretch; gap: 8px; }",
    ".field {",
    "  width: 100%;",
    "  height: 36px;",
    "  padding: 7px 10px;",
    "  border: 1px solid rgb(243 179 75 / 0.4);",
    "  border-radius: 0;",
    "  background: rgb(243 179 75 / 0.04);",
    "  color: #f3b34b;",
    "  outline: none;",
    "  caret-color: transparent;",
    "}",
    ".field::placeholder { color: #9d7330; }",
    ".field:hover, .field:focus { background: rgb(243 179 75 / 0.08); border-color: rgb(243 179 75 / 0.25); }",
    ".block-field { position: relative; width: 100%; }",
    ".block {",
    "  position: absolute;",
    "  top: 50%;",
    "  left: 10px;",
    "  width: 1ch;",
    "  height: 1.15em;",
    "  margin-top: -0.575em;",
    "  background: #f3b34b;",
    "  pointer-events: none;",
    "  animation: block-pulse 1.2s ease-in-out infinite;",
    "}",
    ".block-field:not(:focus-within) .block { visibility: hidden; }",
    ".error { margin: 0; min-height: 1.2em; color: #cc730e; font-size: 13px; }",
    ".close {",
    "  position: absolute;",
    "  top: 12px;",
    "  right: 12px;",
    "  height: 28px;",
    "  padding: 0 8px;",
    "  color: #9d7330;",
    "  font-size: 13px;",
    "}",
    ".close:hover { color: #f3b34b; }",
    ":host([dismissable]) { position: fixed; inset: 0; z-index: 40; }",
    "@keyframes block-pulse { 0%, 100% { opacity: 1; } 50% { opacity: 0.15; } }",
    "@media (prefers-reduced-motion: reduce) { .block { animation: none; } }"
  ].join("\n")

  class DottieLogin extends HTMLElement {
    static get observedAttributes() { return ["name", "dismissable"] }

    constructor() {
      super()
      this._ready = false
      this._idValue = ""
      this._error = ""
      this._busy = false
    }

    connectedCallback() {
      if (this._ready) return
      this._ready = true
      var root = this.attachShadow({ mode: "open" })
      root.innerHTML = "<style>" + style + "</style>"
        + "<button class=\"close\" type=\"button\" hidden>Close</button>"
        + "<div class=\"name\"><button class=\"chip selected\" type=\"button\" disabled></button></div>"
        + "<form class=\"form\" autocomplete=\"on\">"
        + "<div class=\"block-field\"><input class=\"field\" name=\"username\" autocomplete=\"username\" placeholder=\"Almanac ID\" aria-label=\"Almanac ID\" spellcheck=\"false\" required><span class=\"block\" aria-hidden=\"true\"></span></div>"
        + "<div class=\"block-field\"><input class=\"field\" name=\"password\" type=\"password\" autocomplete=\"current-password\" placeholder=\"Write Key\" aria-label=\"Write Key\" spellcheck=\"false\" required><span class=\"block\" aria-hidden=\"true\"></span></div>"
        + "<button class=\"chip bordered\" type=\"submit\">Connect</button>"
        + "<p class=\"error\" role=\"alert\" hidden></p>"
        + "</form>"
      this._title = root.querySelector(".chip.selected")
      this._id = root.querySelector("input[name=\"username\"]")
      this._key = root.querySelector("input[name=\"password\"]")
      this._button = root.querySelector("button[type=\"submit\"]")
      this._status = root.querySelector(".error")
      this._close = root.querySelector(".close")
      this._close.hidden = !this.hasAttribute("dismissable")
      this._title.textContent = this.name
      if (this._idValue) this._id.value = this._idValue
      this.error = this._error
      this.busy = this._busy
      var blocks = root.querySelectorAll(".block-field")
      for (var i = 0; i < blocks.length; i++) this._watch(blocks[i])
      var self = this
      root.querySelector("form").addEventListener("submit", function (event) {
        event.preventDefault()
        var id = self._id.value.replace(/^\s+|\s+$/g, "")
        var key = self._key.value.replace(/^\s+|\s+$/g, "")
        if (!id) { self.error = "Almanac ID is required"; return }
        if (!key) { self.error = "Write Key is required"; return }
        self.error = ""
        self.dispatchEvent(new CustomEvent("connect", {
          detail: { id: id, key: key },
          bubbles: true,
          composed: true
        }))
      })
      this._close.addEventListener("click", function () { self._dismiss() })
      root.addEventListener("keydown", function (event) {
        if (event.key !== "Escape" || !self.hasAttribute("dismissable")) return
        event.stopPropagation()
        self._dismiss()
      })
    }

    attributeChangedCallback(name) {
      if (name === "name" && this._title) this._title.textContent = this.name
      if (name === "dismissable" && this._close) this._close.hidden = !this.hasAttribute("dismissable")
    }

    _dismiss() {
      this.dispatchEvent(new CustomEvent("dismiss", { bubbles: true, composed: true }))
    }

    get name() {
      return this.getAttribute("name") || "Notes"
    }

    set name(value) {
      this.setAttribute("name", value || "Notes")
    }

    get idValue() {
      return this._id ? this._id.value : this._idValue
    }

    set idValue(value) {
      this._idValue = value || ""
      if (this._id) this._id.value = this._idValue
    }

    get error() {
      return this._error
    }

    set error(value) {
      this._error = value || ""
      if (!this._status) return
      this._status.hidden = !this._error
      this._status.textContent = this._error
    }

    get busy() {
      return this._busy
    }

    set busy(value) {
      this._busy = !!value
      if (this._button) this._button.disabled = this._busy
    }

    clearKey() {
      if (this._key) this._key.value = ""
    }

    focus() {
      if (this._id) this._id.focus()
    }

    _watch(wrap) {
      var input = wrap.querySelector("input")
      var block = wrap.querySelector(".block")
      var place = function () {
        var pos = input.selectionStart
        var at = pos == null ? input.value.length : pos
        block.style.left = "calc(10px + " + at + "ch)"
      }
      ;["focus", "input", "keyup", "click", "select"].forEach(function (type) {
        input.addEventListener(type, place)
      })
    }
  }

  customElements.define("dottie-login", DottieLogin)
})()
