import { createSignal } from "solid-js";

const REPO = "https://github.com/peculiarnewbie/carbon-disc";

const shots = [
  { src: "/editing.webp", alt: "Trimming a clip on the timeline", width: 1551, height: 1128 },
  {
    src: "/estimate.webp",
    alt: "A measured size estimate in the settings panel",
    width: 1550,
    height: 1122,
  },
  { src: "/crop.webp", alt: "Crop handles over the frame preview", width: 1552, height: 1121 },
  { src: "/queue.webp", alt: "Two encodes waiting in the queue", width: 1548, height: 1120 },
];

// Warm the cache for all screenshots so arrow clicks swap instantly.
for (const shot of shots) {
  new Image().src = shot.src;
}

const features = [
  "Trim & crop",
  "Size estimate before encoding",
  "Hardware encoders",
  "Queue & batch",
];

export default function App() {
  const [shot, setShot] = createSignal(0);
  const [loaded, setLoaded] = createSignal(true);
  const step = (delta: number) => {
    setLoaded(false);
    setShot((i) => (i + delta + shots.length) % shots.length);
  };

  return (
    <div class="page">
      <header class="top">
        <svg
          class="logo"
          aria-hidden="true"
          xmlns="http://www.w3.org/2000/svg"
          viewBox="-10 -10 120 120"
        >
          <defs>
            <mask id="d">
              <rect x="-10" y="-10" width="120" height="120" fill="#fff" />
              <circle cx="50" cy="50" r="15" fill="#000" />
              <circle cx="32.37" cy="74.27" r="5.5" fill="#000" />
              <circle cx="78.53" cy="40.73" r="5.5" fill="#000" />
              <circle cx="67.63" cy="74.27" r="5.5" fill="#000" />
              <path
                d="M 6.15 39.88 A 45 45 0 0 1 53.92 5.17"
                stroke="#000"
                stroke-width="35"
                fill="none"
                stroke-linecap="round"
              />
            </mask>
          </defs>
          <circle cx="50" cy="50" r="46" fill="#8b5cf6" mask="url(#d)" />
          <path
            d="M 6.15 39.88 A 45 45 0 0 1 53.92 5.17"
            stroke="#a78bfa"
            stroke-width="30"
            fill="none"
            stroke-linecap="round"
          />
        </svg>
        <span class="mark">CARBON DISC</span>
        <a class="github" href={REPO} target="_blank" rel="noreferrer">
          GitHub ↗
        </a>
      </header>

      <main class="hero">
        <h1>Yet another FFmpeg wrapper</h1>
        <p>Open source, built with Rust and egui</p>
        <div class="cta">
          <a class="primary" href={`${REPO}/releases/latest`}>
            Download
          </a>
          <a class="secondary" href={REPO}>
            View the source
          </a>
        </div>
      </main>

      <section class="shot">
        <button class="nav" onClick={() => step(-1)} aria-label="Previous screenshot">
          ‹
        </button>
        <div
          class="frame"
          style={{ "--ar": String(shots[shot()].width / shots[shot()].height) }}
        >
          <img
            src={shots[shot()].src}
            alt={shots[shot()].alt}
            width={shots[shot()].width}
            height={shots[shot()].height}
            class={{ loading: !loaded() }}
            onLoad={() => setLoaded(true)}
          />
        </div>
        <button class="nav" onClick={() => step(1)} aria-label="Next screenshot">
          ›
        </button>
      </section>

      <ul class="features">
        {features.map((feature) => (
          <li>{feature}</li>
        ))}
      </ul>

      <footer>MIT · Screenshot: Big Buck Bunny, © Blender Foundation (CC-BY)</footer>
    </div>
  );
}
