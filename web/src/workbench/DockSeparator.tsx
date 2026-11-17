import type { RefObject } from 'preact';
import { useEffect, useRef, useState } from 'preact/hooks';

export function DockSeparator({ container, dock, hidden }: { container: RefObject<HTMLDivElement>; dock: 'right' | 'bottom'; hidden: boolean }) {
  const [narrow, setNarrow] = useState(() => window.innerWidth < 1200);
  const [width, setWidth] = useState(400);
  const [height, setHeight] = useState(272);
  const drag = useRef(false);
  const bottom = dock === 'bottom' || narrow;
  const [extent, setExtent] = useState({ width: window.innerWidth, height: window.innerHeight });
  useEffect(() => {
    const update = () => {
      setNarrow(window.innerWidth < 1200);
      if (container.current) setExtent({ width: container.current.clientWidth, height: container.current.clientHeight });
    };
    update();
    const observer = typeof ResizeObserver !== 'undefined' ? new ResizeObserver(update) : null;
    if (container.current) observer?.observe(container.current);
    window.addEventListener('resize', update);
    return () => { observer?.disconnect(); window.removeEventListener('resize', update); };
  }, []);
  const max = bottom ? Math.max(160, Math.min(520, Math.round(extent.height * .65))) : Math.max(280, Math.min(600, extent.width - 320));
  const min = bottom ? 160 : 280;
  const value = Math.max(min, Math.min(max, bottom ? height : width));
  useEffect(() => {
    container.current?.style.setProperty(bottom ? '--wb-terminal-height' : '--wb-terminal-width', `${value}px`);
  }, [bottom, value]);
  const resize = (next: number) => (bottom ? setHeight : setWidth)(Math.round(Math.max(min, Math.min(max, next))));
  return <div className="wb-dock-separator" role="separator" tabIndex={0} aria-label={bottom ? '调整终端高度' : '调整终端宽度'} aria-orientation={bottom ? 'horizontal' : 'vertical'} aria-valuemin={min} aria-valuemax={max} aria-valuenow={value} hidden={hidden}
    onPointerDown={event => { event.preventDefault(); drag.current = true; event.currentTarget.setPointerCapture(event.pointerId); }}
    onPointerMove={event => { if (!drag.current || !container.current) return; const rect = container.current.getBoundingClientRect(); resize(bottom ? rect.bottom - event.clientY : rect.right - event.clientX); }}
    onPointerUp={() => { drag.current = false; }} onPointerCancel={() => { drag.current = false; }} onLostPointerCapture={() => { drag.current = false; }}
    onKeyDown={event => {
      const increase = bottom ? 'ArrowUp' : 'ArrowLeft', decrease = bottom ? 'ArrowDown' : 'ArrowRight';
      if (event.key !== increase && event.key !== decrease && event.key !== 'Home' && event.key !== 'End') return;
      event.preventDefault(); resize(event.key === 'Home' ? min : event.key === 'End' ? max : value + (event.key === increase ? 16 : -16));
    }} />;
}
