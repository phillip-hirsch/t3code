//! Which footer controls fit beside the model picker (`resolveRestingComposerControlsLayout` for
//! the composer's one resting block, the permissions chip).

/// How the mode block shows: labelled, icons only, or folded into the "More composer controls" menu.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) enum ModeFit {
    #[default]
    Label,
    Icon,
    Hidden,
}

impl ModeFit {
    const fn step(self) -> u8 {
        match self {
            Self::Label => 0,
            Self::Icon => 1,
            Self::Hidden => 2,
        }
    }
}

/// Natural widths, in px, of what the footer controls row would hold at full size.
#[derive(Clone, Copy, Debug)]
pub(super) struct ControlsWidths {
    pub picker: f32,
    pub block: f32,
    pub block_icon_only: f32,
    pub overflow: f32,
    pub gap: f32,
}

/// `RESTING_CONTROLS_SLACK_PX`: 1px of slack before promoting back, so natural-width jitter can't flap.
const SLACK: f32 = 1.;

pub(super) fn resolve_mode_fit(widths: ControlsWidths, host: f32, previous: ModeFit) -> ModeFit {
    let width_at = |fit: ModeFit| match fit {
        ModeFit::Label => widths.picker + widths.block + widths.gap,
        ModeFit::Icon => widths.picker + widths.block_icon_only + widths.gap,
        ModeFit::Hidden => widths.picker + widths.overflow + widths.gap,
    };
    [ModeFit::Label, ModeFit::Icon]
        .into_iter()
        .find(|&fit| width_at(fit) <= host - if fit.step() < previous.step() { SLACK } else { 0. })
        .unwrap_or(ModeFit::Hidden)
}

#[cfg(test)]
mod tests {
    use super::*;

    const WIDTHS: ControlsWidths =
        ControlsWidths { picker: 140., block: 62., block_icon_only: 32., overflow: 34., gap: 2. };

    #[test]
    fn labels_fold_to_icons_then_into_the_menu_as_the_row_narrows() {
        assert_eq!(resolve_mode_fit(WIDTHS, 400., ModeFit::Label), ModeFit::Label);
        assert_eq!(resolve_mode_fit(WIDTHS, 204., ModeFit::Label), ModeFit::Label);
        assert_eq!(resolve_mode_fit(WIDTHS, 203., ModeFit::Label), ModeFit::Icon);
        assert_eq!(resolve_mode_fit(WIDTHS, 174., ModeFit::Label), ModeFit::Icon);
        assert_eq!(resolve_mode_fit(WIDTHS, 173., ModeFit::Label), ModeFit::Hidden);
    }

    #[test]
    fn promotion_waits_for_one_spare_pixel() {
        assert_eq!(resolve_mode_fit(WIDTHS, 204., ModeFit::Icon), ModeFit::Icon);
        assert_eq!(resolve_mode_fit(WIDTHS, 205., ModeFit::Icon), ModeFit::Label);
        assert_eq!(resolve_mode_fit(WIDTHS, 174., ModeFit::Hidden), ModeFit::Hidden);
        assert_eq!(resolve_mode_fit(WIDTHS, 175., ModeFit::Hidden), ModeFit::Icon);
    }
}
