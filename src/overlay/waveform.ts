// Live waveform on a canvas (DESIGN 5, "Live waveform"): 32 bars, VU-style envelope, scroll interpolated
// between 30 Hz level events, and the collapse into 5 travelling dots for "processing".
const N = 32;
const W = 120;
const H = 24;
const PITCH = 3.75; // 2px bar + 1.75px gap
const TEXT = [237, 232, 223]; // --pill-text
const TALLY = [255, 106, 61]; // --tally
const EVENT_MS = 33;

export class Waveform {
  private ctx: CanvasRenderingContext2D;
  private target = new Float32Array(N);
  private disp = new Float32Array(N);
  private last = 0;
  private raf = 0;
  private collapseAt = 0;

  constructor(canvas: HTMLCanvasElement) {
    const dpr = window.devicePixelRatio || 1;
    canvas.width = W * dpr;
    canvas.height = H * dpr;
    canvas.style.width = `${W}px`;
    canvas.style.height = `${H}px`;
    this.ctx = canvas.getContext("2d")!;
    this.ctx.setTransform(dpr, 0, 0, dpr, 0, 0);
    this.ctx.lineCap = "round";
  }

  /** dBFS as sent by Rust, no conversion: clamp to [-60, 0], normalise, expand the speech range. */
  push(db: number) {
    if (this.collapseAt) return; // frozen while processing
    const v = ((Math.min(0, Math.max(-60, db)) + 60) / 60) ** 1.6;
    this.target.copyWithin(0, 1);
    this.disp.copyWithin(0, 1);
    this.target[N - 1] = v;
    this.disp[N - 1] = 0;
    this.last = performance.now();
  }

  start(processing: boolean) {
    if (processing && !this.collapseAt) this.collapseAt = performance.now();
    if (!processing && this.collapseAt) {
      this.collapseAt = 0;
      this.target.fill(0);
      this.disp.fill(0);
    }
    if (!this.raf) this.raf = requestAnimationFrame(this.frame);
  }

  stop() {
    cancelAnimationFrame(this.raf);
    this.raf = 0;
    this.collapseAt = 0;
    this.target.fill(0);
    this.disp.fill(0);
    this.ctx.clearRect(0, 0, W, H);
  }

  private frame = (now: number) => {
    const { ctx } = this;
    ctx.clearRect(0, 0, W, H);
    const cy = H / 2;
    if (this.collapseAt) this.drawCollapse(now, cy);
    else {
      const p = Math.min(1, (now - this.last) / EVENT_MS);
      const shift = PITCH * (1 - p);
      for (let i = 0; i < N; i++) {
        const t = this.target[i];
        this.disp[i] += (t - this.disp[i]) * (t > this.disp[i] ? 0.55 : 0.18);
        const h = 2 + this.disp[i] * (H - 2);
        const k = i >= N - 4 ? ((i - (N - 5)) / 4) * 0.6 : 0;
        ctx.strokeStyle = mix(k);
        this.bar(i * PITCH + 1 + shift, cy, h, 2);
      }
    }
    this.raf = requestAnimationFrame(this.frame);
  };

  private drawCollapse(now: number, cy: number) {
    const { ctx } = this;
    const k = ease(Math.min(1, (now - this.collapseAt) / 200));
    const slotX = (s: number) => W / 2 + (s - 2) * 9;
    if (k < 1) {
      ctx.strokeStyle = mix(0);
      for (let i = 0; i < N; i++) {
        const x = i * PITCH + 1;
        const h = 2 + this.disp[i] * (H - 2);
        this.bar(x + (slotX(Math.floor((i * 5) / N)) - x) * k, cy, h + (4 - h) * k, 2 + 2 * k);
      }
      return;
    }
    const t = now - this.collapseAt - 200;
    for (let i = 0; i < 5; i++) {
      const a = 0.3 + 0.7 * Math.max(0, Math.sin((t * 2 * Math.PI) / 900 - i * 0.6));
      ctx.strokeStyle = `rgba(${TEXT.join(",")},${a})`;
      this.bar(slotX(i), cy, 4, 4);
    }
  }

  /** A vertical line of total height h (round caps included), mirrored around cy. */
  private bar(x: number, cy: number, h: number, w: number) {
    const half = Math.max(0.01, (h - w) / 2);
    this.ctx.lineWidth = w;
    this.ctx.beginPath();
    this.ctx.moveTo(x, cy - half);
    this.ctx.lineTo(x, cy + half);
    this.ctx.stroke();
  }
}

const ease = (t: number) => 1 - (1 - t) ** 3;
const mix = (k: number) => `rgb(${TEXT.map((c, j) => Math.round(c + (TALLY[j] - c) * k)).join(",")})`;
