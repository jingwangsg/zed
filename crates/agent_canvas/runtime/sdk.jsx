import React, { useState } from 'react';
import { BarChart as RechartsBarChart, LineChart as RechartsLineChart, PieChart as RechartsPieChart, Bar, Line, Pie, Cell, CartesianGrid, XAxis, YAxis, Tooltip, Legend, ResponsiveContainer } from 'recharts';
import { useHostTheme } from './host.jsx';
export { useState, useEffect, useMemo, useRef, useCallback } from 'react';
export { useHostTheme, useCanvasState, useCanvasAction } from './host.jsx';

export function Stack({ children, gap = 16, style }) {
  return <div style={{ display: 'flex', flexDirection: 'column', gap, ...style }}>{children}</div>;
}
export function Row({ children, gap = 12, align = 'center', justify = 'flex-start', wrap = false, style }) {
  return <div style={{ display: 'flex', flexDirection: 'row', gap, alignItems: align, justifyContent: justify, flexWrap: wrap ? 'wrap' : 'nowrap', ...style }}>{children}</div>;
}
export function Grid({ children, columns = 2, gap = 16, style }) {
  return <div style={{ display: 'grid', gridTemplateColumns: typeof columns === 'number' ? 'repeat(' + columns + ', minmax(0, 1fr))' : columns, gap, ...style }}>{children}</div>;
}
export function Spacer() { return <div style={{ flex: 1 }} />; }
export function Divider() { const theme = useHostTheme(); return <hr style={{ border: 0, borderTop: '1px solid ' + theme.stroke.primary, width: '100%', margin: '8px 0' }} />; }
export function H1({ children, style }) { return <h1 style={{ fontSize: 24, lineHeight: '30px', fontWeight: 600, margin: 0, ...style }}>{children}</h1>; }
export function H2({ children, style }) { return <h2 style={{ fontSize: 18, lineHeight: '24px', fontWeight: 600, margin: 0, ...style }}>{children}</h2>; }
export function H3({ children, style }) { return <h3 style={{ fontSize: 16, lineHeight: '22px', fontWeight: 600, margin: 0, ...style }}>{children}</h3>; }
export function Text({ children, size = 'body', muted = false, weight = 'normal', style }) {
  const theme = useHostTheme();
  return <div style={{ fontSize: size === 'small' ? 12 : 14, lineHeight: 1.5, color: muted ? theme.text.secondary : theme.text.primary, fontWeight: weight === 'medium' ? 600 : 400, ...style }}>{children}</div>;
}
export function Code({ children, style }) {
  const theme = useHostTheme();
  return <code style={{ fontFamily: 'ui-monospace, monospace', fontSize: 12, padding: '2px 4px', border: '1px solid ' + theme.stroke.secondary, borderRadius: 3, ...style }}>{children}</code>;
}
export function Link({ href, children }) { const theme = useHostTheme(); return <a href={href} style={{ color: theme.text.link }}>{children}</a>; }
export function Card({ children, style }) {
  const theme = useHostTheme();
  return <section style={{ border: '1px solid ' + theme.stroke.primary, borderRadius: 6, overflow: 'hidden', minWidth: 0, ...style }}>{children}</section>;
}
export function CardHeader({ title, subtitle, trailing, style }) {
  const theme = useHostTheme();
  return <div style={{ display: 'flex', gap: 12, justifyContent: 'space-between', padding: '12px 16px', borderBottom: '1px solid ' + theme.stroke.secondary, ...style }}><div><H3>{title}</H3>{subtitle && <Text size="small" muted>{subtitle}</Text>}</div>{trailing}</div>;
}
export function CardBody({ children, padding = 16, style }) { return <div style={{ padding, ...style }}>{children}</div>; }
export function Stat({ label, value, detail, tone = 'neutral', style }) {
  const theme = useHostTheme();
  return <div style={style}><Text size="small" muted>{label}</Text><div style={{ fontSize: 24, lineHeight: '32px', fontWeight: 600, fontVariantNumeric: 'tabular-nums', color: theme.status[tone] ?? theme.text.primary }}>{value}</div>{detail && <Text size="small" muted>{detail}</Text>}</div>;
}
export function Pill({ children, tone = 'neutral' }) {
  const theme = useHostTheme();
  return <span style={{ display: 'inline-block', padding: '1px 6px', borderRadius: 4, fontSize: 12, border: '1px solid ' + theme.stroke.primary, color: theme.status[tone] ?? theme.text.secondary }}>{children}</span>;
}
export function Callout({ title, children, tone = 'info' }) {
  const theme = useHostTheme();
  return <aside style={{ borderLeft: '3px solid ' + (theme.status[tone] ?? theme.stroke.primary), padding: '8px 12px' }}>{title && <Text weight="medium">{title}</Text>}<Text>{children}</Text></aside>;
}
export function Table({ headers, rows, columnAlign = [], rowTone = [], striped = false, stickyHeader = false }) {
  const theme = useHostTheme();
  return <div style={{ overflowX: 'auto' }}><table><thead style={stickyHeader ? { position: 'sticky', top: 0, background: theme.bg.editor } : undefined}><tr>{headers.map((header, index) => <th key={index} style={{ textAlign: columnAlign[index] ?? 'left', fontSize: 12, color: theme.text.secondary }}>{header}</th>)}</tr></thead><tbody>{rows.map((row, index) => <tr key={index} style={{ color: theme.status[rowTone[index]] ?? theme.text.primary, background: striped && index % 2 ? 'color-mix(in srgb, ' + theme.text.primary + ' 4%, transparent)' : undefined }}>{row.map((cell, column) => <td key={column} style={{ textAlign: columnAlign[column] ?? 'left', fontVariantNumeric: 'tabular-nums' }}>{cell}</td>)}</tr>)}</tbody></table></div>;
}
export function Button({ children, onClick, disabled = false, variant = 'secondary', title }) {
  const theme = useHostTheme();
  return <button type="button" title={title} disabled={disabled} onClick={onClick} style={{ color: variant === 'primary' ? theme.accent.primary : theme.text.primary, opacity: disabled ? 0.5 : 1, cursor: disabled ? 'default' : 'pointer' }}>{children}</button>;
}
export function TextInput({ value, onChange, placeholder, label, disabled = false, type = 'text' }) {
  return <label style={{ display: 'grid', gap: 4 }}>{label && <Text size="small" muted>{label}</Text>}<input aria-label={label ?? placeholder} type={type} value={value} disabled={disabled} placeholder={placeholder} onChange={event => onChange(event.target.value)} /></label>;
}
export function TextArea({ value, onChange, placeholder, label, rows = 3 }) {
  return <label style={{ display: 'grid', gap: 4 }}>{label && <Text size="small" muted>{label}</Text>}<textarea aria-label={label ?? placeholder} rows={rows} value={value} placeholder={placeholder} onChange={event => onChange(event.target.value)} /></label>;
}
export function Select({ value, options, onChange, label, disabled = false }) {
  return <label style={{ display: 'grid', gap: 4 }}>{label && <Text size="small" muted>{label}</Text>}<select aria-label={label} value={value} disabled={disabled} onChange={event => onChange(event.target.value)}>{options.map(option => <option key={option.value} value={option.value}>{option.label}</option>)}</select></label>;
}
export function Checkbox({ checked, onChange, label, disabled = false }) {
  return <label style={{ display: 'inline-flex', alignItems: 'center', gap: 8 }}><input type="checkbox" checked={checked} disabled={disabled} onChange={event => onChange(event.target.checked)} />{label}</label>;
}
export function Toggle(props) { return <Checkbox {...props} />; }
export function CollapsibleSection({ title, children, defaultOpen = true }) {
  const [open, setOpen] = useState(defaultOpen);
  return <section><button type="button" aria-expanded={open} onClick={() => setOpen(!open)} style={{ width: '100%', textAlign: 'left', border: 0, fontWeight: 600 }}>{open ? '▾ ' : '▸ '}{title}</button>{open && <div style={{ paddingTop: 12 }}>{children}</div>}</section>;
}

function chartData(categories, series) {
  if (!series.every(series => series.values.length === categories.length && series.values.every(Number.isFinite))) throw new Error('Each chart series must contain one finite value per category.');
  return categories.map((category, index) => Object.fromEntries([['category', category], ...series.map((series, column) => ['series' + column, series.values[index]])]));
}
export function BarChart({ categories, series, height = 280, stacked = false, horizontal = false, xLabel, yLabel, valueSuffix = '' }) {
  const theme = useHostTheme();
  const data = chartData(categories, series);
  return <div style={{ height, minWidth: 0 }} role="img" aria-label={[xLabel, yLabel].filter(Boolean).join(' / ')}><ResponsiveContainer width="100%" height="100%"><RechartsBarChart data={data} layout={horizontal ? 'vertical' : 'horizontal'} margin={{ top: 12, right: 24, bottom: 24, left: 12 }}><CartesianGrid stroke={theme.stroke.secondary} vertical={false} /><XAxis dataKey={horizontal ? undefined : 'category'} type={horizontal ? 'number' : 'category'} stroke={theme.text.secondary} label={{ value: xLabel, position: 'bottom', offset: 8 }} /><YAxis dataKey={horizontal ? 'category' : undefined} type={horizontal ? 'category' : 'number'} stroke={theme.text.secondary} label={{ value: yLabel, angle: -90, position: 'insideLeft' }} /><Tooltip contentStyle={{ background: theme.bg.editor, borderColor: theme.stroke.primary }} formatter={value => String(value) + valueSuffix} /><Legend verticalAlign="top" wrapperStyle={{ paddingBottom: 12 }} />{series.map((series, index) => <Bar key={index} dataKey={'series' + index} name={series.name} fill={series.color ?? theme.category[index % theme.category.length]} stackId={stacked ? 'total' : undefined} isAnimationActive={false} />)}</RechartsBarChart></ResponsiveContainer></div>;
}
export function LineChart({ categories, series, height = 280, xLabel, yLabel, valueSuffix = '' }) {
  const theme = useHostTheme();
  const data = chartData(categories, series);
  return <div style={{ height, minWidth: 0 }} role="img" aria-label={[xLabel, yLabel].filter(Boolean).join(' / ')}><ResponsiveContainer width="100%" height="100%"><RechartsLineChart data={data} margin={{ top: 12, right: 24, bottom: 24, left: 12 }}><CartesianGrid stroke={theme.stroke.secondary} vertical={false} /><XAxis dataKey="category" stroke={theme.text.secondary} label={{ value: xLabel, position: 'bottom', offset: 8 }} /><YAxis stroke={theme.text.secondary} label={{ value: yLabel, angle: -90, position: 'insideLeft' }} /><Tooltip contentStyle={{ background: theme.bg.editor, borderColor: theme.stroke.primary }} formatter={value => String(value) + valueSuffix} /><Legend verticalAlign="top" wrapperStyle={{ paddingBottom: 12 }} />{series.map((series, index) => <Line key={index} type="linear" dataKey={'series' + index} name={series.name} stroke={series.color ?? theme.category[index % theme.category.length]} dot={false} isAnimationActive={false} />)}</RechartsLineChart></ResponsiveContainer></div>;
}
export function PieChart({ data, height = 260, donut = false }) {
  const theme = useHostTheme();
  if (!data.every(item => Number.isFinite(item.value) && item.value >= 0)) throw new Error('Pie chart values must be finite and non-negative.');
  return <div style={{ height }}><ResponsiveContainer width="100%" height="100%"><RechartsPieChart><Pie data={data} dataKey="value" nameKey="label" innerRadius={donut ? '50%' : 0} outerRadius="75%" isAnimationActive={false}>{data.map((item, index) => <Cell key={index} fill={item.color ?? theme.category[index % theme.category.length]} />)}</Pie><Tooltip contentStyle={{ background: theme.bg.editor, borderColor: theme.stroke.primary }} /><Legend /></RechartsPieChart></ResponsiveContainer></div>;
}
export function DiffStats({ added, removed }) {
  return <Row gap={8}><Pill tone="success">+{added}</Pill><Pill tone="danger">−{removed}</Pill></Row>;
}
export function DiffView({ lines }) {
  const theme = useHostTheme();
  return <pre style={{ margin: 0, fontSize: 12 }}>{lines.map((line, index) => <div key={index} style={{ padding: '2px 12px', color: line.type === 'added' ? theme.status.success : line.type === 'removed' ? theme.status.danger : theme.text.secondary }}>{line.type === 'added' ? '+ ' : line.type === 'removed' ? '− ' : '  '}{line.content}</div>)}</pre>;
}
export function TodoList({ items, onChange }) {
  return <Stack gap={8}>{items.map(item => <Checkbox key={item.id} label={item.label} checked={item.completed} disabled={!onChange} onChange={completed => onChange?.(items.map(other => other.id === item.id ? { ...other, completed } : other))} />)}</Stack>;
}
export function UsageBar({ segments, total, height = 12 }) {
  const theme = useHostTheme();
  const sum = segments.reduce((sum, segment) => sum + segment.value, 0);
  if (!segments.every(segment => Number.isFinite(segment.value) && segment.value >= 0) || (total !== undefined && (!Number.isFinite(total) || total < sum))) throw new Error('Usage bar requires non-negative segments and a total at least as large as their sum.');
  const maximum = total ?? sum;
  return <Stack gap={8}><div style={{ height, display: 'flex', borderRadius: 4, overflow: 'hidden', background: theme.stroke.secondary }}>{segments.map((segment, index) => <div key={index} title={segment.label + ': ' + segment.value} style={{ width: (maximum ? segment.value / maximum * 100 : 0) + '%', background: segment.color ?? theme.category[index % theme.category.length] }} />)}</div><Row wrap>{segments.map((segment, index) => <Text key={index} size="small">{segment.label}: {segment.value}</Text>)}</Row></Stack>;
}

export function computeDAGLayout({ nodes, edges, nodeWidth = 160, nodeHeight = 64, rankGap = 80, nodeGap = 24 }) {
  const incoming = new Map(nodes.map(node => [node.id, 0]));
  if (incoming.size !== nodes.length) throw new Error('DAG node IDs must be unique.');
  const outgoing = new Map(nodes.map(node => [node.id, []]));
  for (const edge of edges) {
    if (!incoming.has(edge.source) || !incoming.has(edge.target)) throw new Error('DAG edges must reference existing nodes.');
    incoming.set(edge.target, incoming.get(edge.target) + 1);
    outgoing.get(edge.source).push(edge.target);
  }
  const queue = nodes.filter(node => incoming.get(node.id) === 0).map(node => node.id);
  const ranks = new Map(queue.map(id => [id, 0]));
  for (let index = 0; index < queue.length; index++) {
    const id = queue[index];
    for (const target of outgoing.get(id)) {
      ranks.set(target, Math.max(ranks.get(target) ?? 0, ranks.get(id) + 1));
      incoming.set(target, incoming.get(target) - 1);
      if (incoming.get(target) === 0) queue.push(target);
    }
  }
  if (queue.length !== nodes.length) throw new Error('DAG layout requires a graph without cycles.');
  const counts = new Map();
  const positions = nodes.map(node => {
    const rank = ranks.get(node.id);
    const column = counts.get(rank) ?? 0;
    counts.set(rank, column + 1);
    return { ...node, x: column * (nodeWidth + nodeGap), y: rank * (nodeHeight + rankGap), width: nodeWidth, height: nodeHeight };
  });
  return { nodes: positions, edges, width: Math.max(0, ...positions.map(node => node.x + node.width)), height: Math.max(0, ...positions.map(node => node.y + node.height)) };
}
