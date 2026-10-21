// Plugin builds resolve `@tuff/panel-ui` here: the shell's component kit.
import type { Shared } from "../shared";

const ui = (globalThis as unknown as { __tuffPanel: Shared }).__tuffPanel.Ui;

export type { Column, FieldProps, LineChartProps, TableProps, XY } from "../ui";
export const { Button, Field, LineChart, Stat, Table, linePath, pageOf } = ui;
