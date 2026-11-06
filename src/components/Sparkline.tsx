import React from "react";

/* Tiny inline-SVG sparkline. No chart library. */
interface SparklineProps {
  values: number[];
  width?: number;
  height?: number;
  stroke?: string;
}

export const Sparkline: React.FC<SparklineProps> = ({
  values,
  width = 120,
  height = 28,
  stroke = "var(--accent)",
}) => {
  if (values.length < 2) {
    return (
      <div
        style={{ width, height }}
        className="flex items-center text-[11px] text-subtle font-mono"
      >
        <span className="mx-auto">—</span>
      </div>
    );
  }
  const max = Math.max(...values, 0.001);
  const min = Math.min(...values, 0);
  const span = Math.max(max - min, 0.001);
  const step = width / (values.length - 1);
  const pts = values.map((v, i) => {
    const x = i * step;
    const y = height - 2 - ((v - min) / span) * (height - 4);
    return `${x.toFixed(1)},${y.toFixed(1)}`;
  });

  return (
    <svg width={width} height={height} viewBox={`0 0 ${width} ${height}`} aria-hidden="true">
      <polyline
        points={pts.join(" ")}
        fill="none"
        stroke={stroke}
        strokeWidth="1.5"
        strokeLinejoin="round"
        strokeLinecap="round"
      />
    </svg>
  );
};
