// CLI twin of `bench/cli/mersey/semver.mersey`. No JS runtime ships semver, so
// `std/semver.mersey` is ported here line for line — what is compared is two
// engines running one program.
function digitVal(cp) {
  if (cp == null || cp < 48 || cp > 57) {
    return -1;
  }
  return cp - 48;
}

function numericId(s) {
  if (s.length === 0) {
    return null;
  }
  if (s.length > 1 && s.charAt(0) === "0") {
    return null;
  }
  let v = 0;
  for (let i = 0; i < s.length; i += 1) {
    const d = digitVal(s.codePointAt(i));
    if (d < 0) {
      return null;
    }
    v = v * 10 + d;
  }
  return v;
}

function identChars(s) {
  for (let i = 0; i < s.length; i += 1) {
    const cp = s.codePointAt(i);
    if (cp == null) {
      return false;
    }
    const isDigit = cp >= 48 && cp <= 57;
    const isUpper = cp >= 65 && cp <= 90;
    const isLower = cp >= 97 && cp <= 122;
    const isDash = cp === 45;
    if (!(isDigit || isUpper || isLower || isDash)) {
      return false;
    }
  }
  return true;
}

function isNumeric(s) {
  if (s.length === 0) {
    return false;
  }
  for (let i = 0; i < s.length; i += 1) {
    if (digitVal(s.codePointAt(i)) < 0) {
      return false;
    }
  }
  return true;
}

function validIdentifiers(s, build) {
  if (s.length === 0) {
    return false;
  }
  for (const id of s.split(".")) {
    if (id.length === 0) {
      return false;
    }
    if (!identChars(id)) {
      return false;
    }
    if (!build && isNumeric(id) && id.length > 1 && id.charAt(0) === "0") {
      return false;
    }
  }
  return true;
}

function lexCmp(a, b) {
  const n = a.length < b.length ? a.length : b.length;
  for (let i = 0; i < n; i += 1) {
    const ca = a.codePointAt(i);
    const cb = b.codePointAt(i);
    const va = ca == null ? 0 : ca;
    const vb = cb == null ? 0 : cb;
    if (va !== vb) {
      return va < vb ? -1 : 1;
    }
  }
  if (a.length === b.length) {
    return 0;
  }
  return a.length < b.length ? -1 : 1;
}

function compareIdent(a, b) {
  const an = isNumeric(a);
  const bn = isNumeric(b);
  if (an && bn) {
    if (a.length !== b.length) {
      return a.length < b.length ? -1 : 1;
    }
    return lexCmp(a, b);
  }
  if (an) {
    return -1;
  }
  if (bn) {
    return 1;
  }
  return lexCmp(a, b);
}

function comparePre(a, b) {
  if (a === b) {
    return 0;
  }
  if (a.length === 0) {
    return 1;
  }
  if (b.length === 0) {
    return -1;
  }
  const ai = a.split(".");
  const bi = b.split(".");
  const n = ai.length < bi.length ? ai.length : bi.length;
  for (let i = 0; i < n; i += 1) {
    const c = compareIdent(ai[i], bi[i]);
    if (c !== 0) {
      return c;
    }
  }
  if (ai.length === bi.length) {
    return 0;
  }
  return ai.length < bi.length ? -1 : 1;
}

class Version {
  constructor(major, minor, patch, prerelease, build) {
    this.major = major;
    this.minor = minor;
    this.patch = patch;
    this.prerelease = prerelease;
    this.build = build;
  }

  static parse(text) {
    let s = text.trim();
    if (s.startsWith("v") || s.startsWith("V")) {
      s = s.slice(1);
    }
    let build = "";
    const plus = s.indexOf("+");
    if (plus >= 0) {
      build = s.slice(plus + 1);
      s = s.slice(0, plus);
      if (!validIdentifiers(build, true)) {
        return null;
      }
    }
    let pre = "";
    const dash = s.indexOf("-");
    if (dash >= 0) {
      pre = s.slice(dash + 1);
      s = s.slice(0, dash);
      if (!validIdentifiers(pre, false)) {
        return null;
      }
    }
    const parts = s.split(".");
    if (parts.length !== 3) {
      return null;
    }
    const major = numericId(parts[0]);
    const minor = numericId(parts[1]);
    const patch = numericId(parts[2]);
    if (major == null || minor == null || patch == null) {
      return null;
    }
    return new Version(major, minor, patch, pre, build);
  }

  compare(other) {
    if (this.major !== other.major) {
      return this.major < other.major ? -1 : 1;
    }
    if (this.minor !== other.minor) {
      return this.minor < other.minor ? -1 : 1;
    }
    if (this.patch !== other.patch) {
      return this.patch < other.patch ? -1 : 1;
    }
    return comparePre(this.prerelease, other.prerelease);
  }

  core() {
    return `${this.major}.${this.minor}.${this.patch}`;
  }

  toString() {
    let s = this.core();
    if (this.prerelease.length > 0) {
      s = `${s}-${this.prerelease}`;
    }
    if (this.build.length > 0) {
      s = `${s}+${this.build}`;
    }
    return s;
  }

  satisfies(range) {
    const r = range.trim();
    if (r.length === 0 || r === "*") {
      return true;
    }
    if (r.startsWith("^")) {
      const base = Version.parse(r.slice(1));
      if (base == null) {
        return false;
      }
      return this.compare(base) >= 0 && this.compare(caretUpper(base)) < 0;
    }
    if (r.startsWith("~")) {
      const base = Version.parse(r.slice(1));
      if (base == null) {
        return false;
      }
      const upper = new Version(base.major, base.minor + 1, 0, "", "");
      return this.compare(base) >= 0 && this.compare(upper) < 0;
    }
    let op = "=";
    let rest = r;
    if (r.startsWith(">=")) {
      op = ">="; rest = r.slice(2);
    } else if (r.startsWith("<=")) {
      op = "<="; rest = r.slice(2);
    } else if (r.startsWith(">")) {
      op = ">"; rest = r.slice(1);
    } else if (r.startsWith("<")) {
      op = "<"; rest = r.slice(1);
    } else if (r.startsWith("=")) {
      op = "="; rest = r.slice(1);
    }
    const target = Version.parse(rest.trim());
    if (target == null) {
      return false;
    }
    const c = this.compare(target);
    if (op === ">=") { return c >= 0; }
    if (op === "<=") { return c <= 0; }
    if (op === ">") { return c > 0; }
    if (op === "<") { return c < 0; }
    return c === 0;
  }
}

function caretUpper(base) {
  if (base.major > 0) {
    return new Version(base.major + 1, 0, 0, "", "");
  }
  if (base.minor > 0) {
    return new Version(0, base.minor + 1, 0, "", "");
  }
  return new Version(0, 0, base.patch + 1, "", "");
}

function valid(text) {
  return Version.parse(text) != null;
}

function compare(a, b) {
  const va = Version.parse(a);
  const vb = Version.parse(b);
  if (va == null || vb == null) {
    return null;
  }
  return va.compare(vb);
}

function satisfies(version, range) {
  const v = Version.parse(version);
  if (v == null) {
    return null;
  }
  return v.satisfies(range);
}

function work(n) {
  let sum = 0;
  for (let i = 0; i < n; i += 1) {
    const a = `${i % 9}.${i % 17}.${i % 5}-alpha.${i % 7}+build.${i}`;
    const b = `${(i + 1) % 9}.${i % 17}.${i % 5}`;

    const va = Version.parse(a);
    if (va != null) {
      sum = (sum + va.toString().length) % 1000003;
      sum = (sum + va.core().length) % 1000003;
    }
    const c = compare(a, b);
    if (c != null) {
      sum = (sum + c + 2) % 1000003;
    }
    if (valid(b)) {
      sum = (sum + 1) % 1000003;
    }
    const s = satisfies(b, `^${i % 9}.0.0`);
    if (s != null && s) {
      sum = (sum + 3) % 1000003;
    }
    if (!valid(`${i}.x.${i}`)) {
      sum = (sum + 5) % 1000003;
    }
  }
  return sum;
}

work(1000); // warm up (parity with the .mersey warm-up round)
const t0 = performance.now();
const c = work(20000);
const t1 = performance.now();
console.log(`RESULT semver ${t1 - t0} ${c}`);
