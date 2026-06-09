// About: centered statement card. Heading mixes sans with Fraunces italic;
// body paragraph reveals per-character as you scroll through it.
import { motion, useScroll, useTransform } from 'framer-motion';
import { useRef } from 'react';
import type { MotionValue } from 'framer-motion';
import { WordsPullUpMultiStyle } from './pullup';

const BODY =
  'rekody only listens while you hold the key. Transcription runs on your machine — NVIDIA Nemotron streams words as you speak, Whisper covers 100+ languages — an LLM tidies the grammar, and the result is typed at your cursor about fifty milliseconds after you let go. MIT-licensed, no telemetry, free forever.';

function AnimatedLetter({
  char,
  index,
  total,
  progress,
}: {
  char: string;
  index: number;
  total: number;
  progress: MotionValue<number>;
}) {
  const charProgress = index / total;
  const opacity = useTransform(
    progress,
    [charProgress - 0.1, charProgress + 0.05],
    [0.2, 1],
  );
  return (
    <motion.span style={{ opacity }}>{char}</motion.span>
  );
}

export default function AboutCinematic() {
  const bodyRef = useRef<HTMLParagraphElement>(null);
  const { scrollYProgress } = useScroll({
    target: bodyRef,
    offset: ['start 0.8', 'end 0.2'],
  });
  const chars = BODY.split('');

  return (
    <section className="bg-black px-4 py-20 md:px-6 md:py-28">
      <div
        className="mx-auto flex max-w-6xl flex-col items-center gap-10 rounded-2xl px-6 py-16 text-center md:gap-14 md:rounded-[2rem] md:px-16 md:py-24"
        style={{ backgroundColor: '#101616' }}
      >
        <span
          className="text-[10px] tracking-[0.18em] uppercase sm:text-xs"
          style={{ color: '#20808D', fontFamily: 'var(--font-mono)' }}
        >
          Open source
        </span>
        <h2
          className="mx-auto max-w-3xl text-3xl leading-[0.95] sm:text-4xl sm:leading-[0.9] md:text-5xl lg:text-6xl xl:text-7xl"
          style={{ color: '#FBFAF4' }}
        >
          <WordsPullUpMultiStyle
            segments={[
              { text: 'Your voice becomes text,', className: 'font-normal' },
              {
                text: 'anywhere you can type.',
                className: 'serif-accent',
              },
            ]}
          />
        </h2>
        <p
          ref={bodyRef}
          className="max-w-2xl text-xs sm:text-sm md:text-base"
          style={{ color: '#FBFAF4', lineHeight: 1.7 }}
        >
          {chars.map((char, i) => (
            <AnimatedLetter
              key={i}
              char={char}
              index={i}
              total={chars.length}
              progress={scrollYProgress}
            />
          ))}
        </p>
      </div>
    </section>
  );
}
