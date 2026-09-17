import type { CSSProperties, ReactNode, ReactElement, Dispatch, SetStateAction } from 'react';
export type { CSSProperties, ReactNode } from 'react';
export { useState, useEffect, useMemo, useRef, useCallback } from 'react';

export type Tone = 'neutral' | 'success' | 'warning' | 'danger' | 'info';
export interface CanvasTheme {
  kind: 'light' | 'dark';
  text: { primary: string; secondary: string; tertiary: string; link: string };
  bg: { editor: string; elevated: string };
  stroke: { primary: string; secondary: string; focused: string };
  accent: { primary: string };
  status: { success: string; warning: string; danger: string; info: string };
  category: string[];
}
export function useHostTheme(): CanvasTheme;
export function useCanvasState<T>(key: string, initialValue: T): [T, Dispatch<SetStateAction<T>>];
export type CanvasAction =
  | { type: 'askAgent'; prompt: string }
  | { type: 'openFile'; path: string; line?: number }
  | { type: 'openAgent'; sessionId: string };
/** askAgent appends a draft to the owning conversation; the user sends it. */
export function useCanvasAction(): (action: CanvasAction) => void;

type Content = { children?: ReactNode; style?: CSSProperties };
export function Stack(props: Content & { gap?: number }): ReactElement;
export function Row(props: Content & { gap?: number; align?: CSSProperties['alignItems']; justify?: CSSProperties['justifyContent']; wrap?: boolean }): ReactElement;
export function Grid(props: Content & { columns?: number | string; gap?: number }): ReactElement;
export function Spacer(): ReactElement;
export function Divider(): ReactElement;
export function H1(props: Content): ReactElement;
export function H2(props: Content): ReactElement;
export function H3(props: Content): ReactElement;
export function Text(props: Content & { size?: 'body' | 'small'; muted?: boolean; weight?: 'normal' | 'medium' }): ReactElement;
export function Code(props: Content): ReactElement;
export function Link(props: { href: string; children: ReactNode }): ReactElement;
export function Card(props: Content): ReactElement;
export function CardHeader(props: { title: ReactNode; subtitle?: ReactNode; trailing?: ReactNode; style?: CSSProperties }): ReactElement;
export function CardBody(props: Content & { padding?: number }): ReactElement;
export function Stat(props: { label: ReactNode; value: ReactNode; detail?: ReactNode; tone?: Tone; style?: CSSProperties }): ReactElement;
export function Pill(props: { children: ReactNode; tone?: Tone }): ReactElement;
export function Callout(props: { title?: ReactNode; children: ReactNode; tone?: Tone }): ReactElement;
export function Table(props: { headers: ReactNode[]; rows: ReactNode[][]; columnAlign?: Array<'left' | 'center' | 'right'>; rowTone?: Tone[]; striped?: boolean; stickyHeader?: boolean }): ReactElement;
export function Button(props: { children: ReactNode; onClick?: () => void; disabled?: boolean; variant?: 'primary' | 'secondary'; title?: string }): ReactElement;
export function TextInput(props: { value: string; onChange: (value: string) => void; placeholder?: string; label?: string; disabled?: boolean; type?: 'text' | 'number' | 'search' }): ReactElement;
export function TextArea(props: { value: string; onChange: (value: string) => void; placeholder?: string; label?: string; rows?: number }): ReactElement;
export function Select(props: { value: string; options: Array<{ value: string; label: string }>; onChange: (value: string) => void; label?: string; disabled?: boolean }): ReactElement;
export function Checkbox(props: { checked: boolean; onChange: (value: boolean) => void; label: string; disabled?: boolean }): ReactElement;
export const Toggle: typeof Checkbox;
export function CollapsibleSection(props: { title: ReactNode; children: ReactNode; defaultOpen?: boolean }): ReactElement;

export interface ChartSeries { name: string; values: number[]; color?: string }
export interface ChartProps {
  categories: Array<string | number>;
  series: ChartSeries[];
  height?: number;
  xLabel?: string;
  yLabel?: string;
  valueSuffix?: string;
}
export function BarChart(props: ChartProps & { stacked?: boolean; horizontal?: boolean }): ReactElement;
export function LineChart(props: ChartProps): ReactElement;
export function PieChart(props: { data: Array<{ label: string; value: number; color?: string }>; height?: number; donut?: boolean }): ReactElement;
export function DiffStats(props: { added: number; removed: number }): ReactElement;
export function DiffView(props: { lines: Array<{ type: 'added' | 'removed' | 'unchanged'; content: string }> }): ReactElement;
export interface TodoItem { id: string; label: string; completed: boolean }
export function TodoList(props: { items: TodoItem[]; onChange?: (items: TodoItem[]) => void }): ReactElement;
export function UsageBar(props: { segments: Array<{ label: string; value: number; color?: string }>; total?: number; height?: number }): ReactElement;
export interface DAGNode { id: string; label?: string }
export interface DAGEdge { source: string; target: string }
export interface DAGLayoutNode extends DAGNode { x: number; y: number; width: number; height: number }
export function computeDAGLayout(options: { nodes: DAGNode[]; edges: DAGEdge[]; nodeWidth?: number; nodeHeight?: number; rankGap?: number; nodeGap?: number }): { nodes: DAGLayoutNode[]; edges: DAGEdge[]; width: number; height: number };
