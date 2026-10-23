import { createSignal } from "solid-js";

const REPO = "https://github.com/peculiarnewbie/carbon-disc";

const shots = [
  { src: "/editing.webp", alt: "Trimming a clip on the timeline" },
  { src: "/estimate.webp", alt: "A measured size estimate in the settings panel" },
  { src: "/crop.webp", alt: "Crop handles over the frame preview" },
  { src: "/queue.webp", alt: "Two encodes waiting in the queue" },
];

const features = [
  "Trim & crop",
  "Size estimate before encoding",
  "Hardware encoders",
  "Queue & batch",
];

export default function App() {
  const [shot, setShot] = createSignal(0);
  const step = (delta: number) => setShot((i) => (i + delta + shots.length) % shots.length);

  return (
    <div class="page">
      <header class="top">
        <svg class="logo" viewBox="-10 -10 120 120" aria-hidden="true">
          <defs>
            <mask id="logo-cut">
              <rect x="-10" y="-10" width="120" height="120" fill="#fff" />
              <circle cx="50" cy="50" r="15" fill="#000" />
              <g fill="#000" transform="rotate(36 50 50)">
                <circle cx="50" cy="20" r="5.5" />
                <circle cx="21.5" cy="59.3" r="5.5" />
                <circle cx="32.4" cy="25.7" r="5.5" />
                <circle cx="67.6" cy="25.7" r="5.5" />
                <circle cx="78.5" cy="59.3" r="5.5" />
              </g>
            </mask>
          </defs>
          <circle cx="50" cy="50" r="46" fill="#8b5cf6" mask="url(#logo-cut)" />
          <path
            d="M -3.5 42.5 A 54 54 0 0 1 42.5 -3.5"
            fill="none"
            stroke="#8b5cf6"
            stroke-width="12"
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
        <img src={shots[shot()].src} alt={shots[shot()].alt} />
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
