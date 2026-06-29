# UI Wireframes

This directory contains wireframes for the Alarm Clock application's user interface, showcasing the four default themes (2 base themes × 2 variants):

## Available Themes

### Light Theme

**Base**: [Liquid Glass](./liquid-glass/index.html) and [Neuromorphic](./neuromorphic/index.html)

### Dark Theme

**Variant**: [Dark Liquid Glass](./dark-liquid-glass/index.html) and [Dark Neuromorphic](./dark-neuromorphic/index.html)

---

### 1. Liquid Glass Theme

A modern, frosted glass aesthetic using glassmorphism effects with blurred backgrounds and subtle borders. Best suited for dark environments.

- **Design Style**: Glassmorphism
- **Background**: Dark gradient (deep blues/purples)
- **Key Features**:
  - Frosted glass effect on all cards
  - Soft drop shadows with backdrop blur
  - Bright white text on dark backgrounds
  - Subtle glow effects on interactive elements
  - Elegant transparency

### 2. Neuromorphic Theme

A soft, 3D aesthetic using neumorphism principles with gradient shadows that make elements appear extruded from the interface.

- **Design Style**: Neumorphism
- **Background**: Light gray (#e0e5ec)
- **Key Features**:
  - Soft shadow gradients for depth
  - Elements appear to extrude from the background
  - Consistent color palette throughout
  - Gentle, tactile appearance
  - No hard borders - only shadows define edges

### 3. Dark Liquid Glass Theme

A dark-mode variant of Liquid Glass with enhanced contrast and reduced ambient light.

- **Design Style**: Darkened Glassmorphism
- **Background**: Black (#000000) with subtle radial gradients
- **Key Features**:
  - Darker background with subtle radial gradients
  - Semi-transparent glass cards with backdrop-filter
  - White text with reduced opacity for secondary elements
  - Stronger glow effects for main time display
  - Optimized for OLED displays and nighttime use

### 4. Dark Neuromorphic Theme

A dark-mode variant of Neuromorphic with adjusted shadow gradients for darker backgrounds.

- **Design Style**: Dark Neumorphism
- **Background**: Dark gray (#1a1a1a)
- **Key Features**:
  - Darker base color with adjusted shadow gradients
  - White elements with soft extrusion shadows
  - Reduced contrast for visual comfort
  - Consistent depth cues throughout
  - Maintains tactile feel in dark mode

## Panels

Each theme includes wireframes for all four panels:

1. **Clock Panel** - Main interface with analog clock, weather, and next calendar event
2. **Daily Data Panel** - Today's agenda, tomorrow's forecast, and current conditions
3. **Media Panel** - Now playing display, transport controls, and favorites
4. **Settings Panel** - Theme, alarms, and display configuration

## Files

- `liquid-glass/index.html` - Interactive wireframes for the Liquid Glass theme
- `neuromorphic/index.html` - Interactive wireframes for the Neuromorphic theme
- `dark-liquid-glass/index.html` - Dark-mode variant of Liquid Glass theme
- `dark-neuromorphic/index.html` - Dark-mode variant of Neuromorphic theme

## How to Use

Open the HTML files in any web browser to view and interact with the wireframes. The wireframes demonstrate:
- Layout and spacing
- Component hierarchy
- Navigation between panels
- Theme-specific styling
- Interactive elements (buttons, cards, navigation)

## Theme Selection Guide

| Theme | Best For | Display Type |
|-------|----------|--------------|
| **Liquid Glass** | Modern aesthetic, dark rooms | OLED, LCD |
| **Neuromorphic** | Soft tactile feel, daylight | LCD, Ambient light |
| **Dark Liquid Glass** | Night use, OLED displays | OLED, Dark environments |
| **Dark Neuromorphic** | Low-light tactile feel | LCD, OLED |

## Theme Seam Compatibility

All themes are designed to work with the Slint `ClockFace` component's theming system:

```slint
ClockFace {
    clock-color: #ff6b6b;  /* Accent color (second hand, highlights) */
    font-family: 'Segoe UI';
}
```

The themes use consistent color schemes within the available seam parameters. For complete theming implementation, refer to the [PRD](../../PRD.md) and [Slice 0 Architecture](../changes/slice-0-architecture-skeleton/design.md) documentation.
