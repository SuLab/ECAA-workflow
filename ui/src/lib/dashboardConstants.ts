/**
 * Constants for the scatter/volcano plots in DashboardTab and the diff
 * highlighting in CompareTab. Extracted from inline values to make UX tuning
 * a one-file change.
 */

/** Default scatter / volcano plot canvas dimensions (px). */
export const SCATTER_PLOT_WIDTH = 620;
export const SCATTER_PLOT_HEIGHT = 480;

/** Inner padding before plot axes (px). */
export const SCATTER_AXIS_PADDING_X = 28;
export const SCATTER_AXIS_PADDING_Y = 32;

/** Zoom bounds (unitless scale multiplier). 0.25 = 4x zoomed out; 20 = 20x in. */
export const SCATTER_ZOOM_MIN = 0.25;
export const SCATTER_ZOOM_MAX = 20;

/** Wheel-delta scaling factor for smooth scroll-zoom. Smaller = slower zoom. */
export const SCATTER_WHEEL_ZOOM_FACTOR = 0.0015;

/** Epsilon guard for coordinate-mapping divisions. Smaller than any plausible
 * data range; prevents NaN from data with identical min/max. */
export const COORD_EPSILON = 1e-9;

/** Divisor applied to base point-radius to scale dots with density. */
export const SCATTER_POINT_RADIUS_DIVISOR = 1.4;

/** Fill opacities for scatter dots (background = unselected, foreground = selected). */
export const SCATTER_FILL_OPACITY_BG = 0.1;
export const SCATTER_FILL_OPACITY_FG = 0.75;

/** Above this many categories, legend collapses into a "show all" pill. */
export const MAX_CATEGORICAL_LEGEND_ENTRIES = 20;

/** Concordance-delta threshold for row highlighting in CompareTab.
 * Rows with absolute difference >= this value are highlighted as "diverged". */
export const COMPARE_DIFF_THRESHOLD = 0.1;
