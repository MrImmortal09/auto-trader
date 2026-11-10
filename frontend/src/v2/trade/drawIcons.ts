// Each drawing tool's icon, shared by the phone's Draw menu and the desktop
// toolbar.

import {
  AlignVerticalSpaceAround,
  Brush,
  Equal,
  Flag,
  Minus,
  MoveDiagonal,
  MoveRight,
  MoveUpRight,
  RectangleHorizontal,
  Rows3,
  Ruler,
  SeparatorHorizontal,
  SeparatorVertical,
  Slash,
  Tag,
  Type,
  Waypoints,
  type LucideIcon,
} from 'lucide-react';
import type { DrawTool } from './drawings';

export const DRAW_ICONS: Record<DrawTool, LucideIcon> = {
  segment: Slash,
  rayLine: MoveUpRight,
  straightLine: MoveDiagonal,
  horizontalStraightLine: Minus,
  horizontalRayLine: MoveRight,
  horizontalSegment: SeparatorHorizontal,
  verticalStraightLine: SeparatorVertical,
  priceLine: Tag,
  fibonacciLine: AlignVerticalSpaceAround,
  parallelStraightLine: Equal,
  priceChannelLine: Rows3,
  v2Path: Waypoints,
  v2Rect: RectangleHorizontal,
  brush: Brush,
  v2Text: Type,
  simpleAnnotation: Flag,
  v2Ruler: Ruler,
};

