import SwiftUI

#if os(macOS)
    /// A month of days to click, under a date well while it is being typed
    /// in. Nothing in it takes the keyboard (`focusable(false)`), so the well
    /// keeps its caret: a day can be typed or picked, whichever is quicker.
    struct DayPicker: View {
        /// The day the well reads as now, if it reads.
        let selected: Date?
        /// Days before this cannot be picked (an end before its start).
        var earliest: Date?
        let today: Date
        let calendar: Calendar
        let pick: (Date) -> Void

        @State private var month: Date?

        private var shown: Date {
            let base = month ?? selected ?? today
            return calendar.date(from: calendar.dateComponents([.year, .month], from: base)) ?? base
        }

        var body: some View {
            VStack(alignment: .leading, spacing: Space.s6) {
                header
                HStack(spacing: 0) {
                    ForEach(Array(weekdayLetters.enumerated()), id: \.offset) { _, letter in
                        Text(letter)
                            .font(Typo.ui(10, weight: .medium))
                            .foregroundStyle(Palette.inkFaintest)
                            .frame(width: Self.cell, height: Space.scaled(16))
                    }
                }
                VStack(spacing: Space.s2) {
                    ForEach(0..<6, id: \.self) { row in
                        HStack(spacing: 0) {
                            ForEach(0..<7, id: \.self) { col in
                                dayButton(days[row * 7 + col])
                            }
                        }
                    }
                }
            }
            .padding(Space.s10)
            .background(
                RoundedRectangle(cornerRadius: Radius.well, style: .continuous)
                    .fill(Palette.hairline.opacity(0.35)))
            .onChange(of: selected) { _, _ in month = nil }
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Pick a day")
        }

        private static var cell: CGFloat { Space.scaled(28) }

        private var header: some View {
            HStack(spacing: Space.s6) {
                Text(Agenda.format(shown, "MMMM yyyy", calendar))
                    .font(Typo.ui(12, weight: .semibold))
                    .foregroundStyle(Palette.ink)
                Spacer(minLength: Space.s8)
                step("chevron.left", "Previous month", -1)
                step("chevron.right", "Next month", 1)
            }
            .padding(.horizontal, Space.s4)
        }

        private func step(_ symbol: String, _ help: String, _ by: Int) -> some View {
            Button {
                month = calendar.date(byAdding: .month, value: by, to: shown)
            } label: {
                Image(systemName: symbol)
                    .font(Typo.ui(10, weight: .semibold))
                    .foregroundStyle(Palette.inkDim)
                    .frame(width: Space.scaled(20), height: Space.scaled(20))
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .focusable(false)
            .pointingHand()
            .help(help)
            .accessibilityLabel(help)
        }

        /// Six weeks from the week the month starts in.
        private var days: [Date] {
            let first = shown
            let lead = (calendar.component(.weekday, from: first) - calendar.firstWeekday + 7) % 7
            let start = calendar.date(byAdding: .day, value: -lead, to: first) ?? first
            return (0..<42).map { calendar.date(byAdding: .day, value: $0, to: start) ?? start }
        }

        private var weekdayLetters: [String] {
            let symbols = calendar.veryShortStandaloneWeekdaySymbols
            return (0..<7).map { symbols[(calendar.firstWeekday - 1 + $0) % 7] }
        }

        private func dayButton(_ day: Date) -> some View {
            let isSelected = selected.map { calendar.isDate($0, inSameDayAs: day) } ?? false
            let isToday = calendar.isDate(day, inSameDayAs: today)
            let inMonth = calendar.isDate(day, equalTo: shown, toGranularity: .month)
            let allowed = earliest.map { calendar.startOfDay(for: day) >= calendar.startOfDay(for: $0) } ?? true
            let tone: Color =
                isSelected ? .white : !allowed ? Palette.inkFaintest.opacity(0.5)
                : isToday ? Palette.accentInk : inMonth ? Palette.ink : Palette.inkFaintest
            return Button {
                pick(calendar.startOfDay(for: day))
            } label: {
                Text("\(calendar.component(.day, from: day))")
                    .font(Typo.num(11.5, weight: isSelected || isToday ? .semibold : .regular))
                    .foregroundStyle(tone)
                    .frame(width: Self.cell, height: Space.scaled(24))
                    .background(
                        Circle()
                            .fill(isSelected ? Palette.accent : Color.clear)
                            .frame(width: Space.scaled(24), height: Space.scaled(24)))
                    .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .focusable(false)
            .disabled(!allowed)
            .pointingHand()
            .accessibilityLabel(Agenda.format(day, "EEEE MMMM d", calendar))
            .accessibilityAddTraits(isSelected ? .isSelected : [])
        }
    }
#endif
