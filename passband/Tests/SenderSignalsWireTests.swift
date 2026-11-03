// The sender-signals wire (issue #639) and the one rule the reader adds to
// it: which lines are drawn as warnings.
//
// The JSON is the shape `GET /client/thread/{id}` sends for a message's
// `sender_signals`; an older daemon sends nothing, and a mangled value must
// not cost the reader the whole message (the notification extension decodes
// this type too).

import Foundation

@main
@MainActor
struct SenderSignalsWireTests {
    static var failures = 0
    static var checks = 0

    static func main() {
        aMessageCarriesItsSignals()
        anOlderDaemonDrawsNothing()
        aMangledValueCostsNothingButTheLine()
        onlyTheScamShapesWarn()
        theMostAlarmingPartLeads()
        theDaemonsWarnCallWins()
        relayAddressesShowTheirDomain()
        oneBadFieldKeepsTheRest()
        bidiAndLengthAreCleaned()

        if failures > 0 {
            print("FAILED: \(failures) of \(checks) checks")
            exit(1)
        }
        print("ok: \(checks) checks passed")
    }

    static func expect(_ ok: Bool, _ what: String) {
        checks += 1
        if !ok {
            failures += 1
            print("  FAIL: \(what)")
        }
    }

    static func message(_ extra: String) -> ClientMessage? {
        let body = #"{"id":1,"from_addr":"x@evil.example","from_name":null,"received_at":"2026-10-28T00:00:00Z","content":"hi","html":null"# + extra + "}"
        return try? JSONDecoder().decode(ClientMessage.self, from: Data(body.utf8))
    }

    static func aMessageCarriesItsSignals() {
        let m = message(#","sender_known":false,"sender_signals":{"via":"sendgrid.net","reply_to":"pay@elsewhere.example","envelope_domain":"em.sendgrid.net"}"#)
        expect(m?.sender_signals?.via == "sendgrid.net", "via decodes")
        expect(m?.sender_signals?.reply_to == "pay@elsewhere.example", "reply_to decodes")
        expect(m?.senderLine?.warning == false, "a stranger's foreign reply-to is quiet")
        expect(m?.senderLine?.text.contains("em.sendgrid.net") == false, "the bounce domain is not drawn")
    }

    static func anOlderDaemonDrawsNothing() {
        let m = message("")
        expect(m != nil, "decodes without the field")
        expect(m?.senderLine == nil, "no line")
        let empty = message(#","sender_signals":{}"#)
        expect(empty?.senderLine == nil, "an empty set draws nothing")
    }

    static func aMangledValueCostsNothingButTheLine() {
        let m = message(#","sender_signals":"via sendgrid""#)
        expect(m != nil, "the message still decodes")
        expect(m?.senderLine == nil, "and draws no line")
    }

    static func onlyTheScamShapesWarn() {
        let quiet = SenderSignals(sender: "assistant@corp.example", via: "sendgrid.net")
        expect(quiet.line(senderKnown: false)?.warning == false, "via and sent-by stay quiet")
        let known = SenderSignals(reply_to: "billing@help.example")
        expect(known.line(senderKnown: true)?.warning == false, "a contact's reply-to is quiet")
        expect(known.line(senderKnown: false)?.warning == false, "nor is a stranger's")
        let spoof = SenderSignals(name_spoof: "support@paypal.com")
        expect(spoof.line(senderKnown: true)?.warning == true, "a name spoof always warns")
    }

    static func theMostAlarmingPartLeads() {
        let all = SenderSignals(
            sender: "relay@evil.example", via: "sendgrid.net",
            reply_to: "pay@elsewhere.example", name_spoof: "billing@bank.example")
        let text = all.line(senderKnown: false)?.text ?? ""
        expect(
            text == "Name claims billing@bank.example · Replies go to pay@elsewhere.example · Sent by relay@evil.example · via sendgrid.net",
            "order: \(text)")
        expect(all.line(senderKnown: false)?.parts.count == 4, "one row per signal")
    }

    static func theDaemonsWarnCallWins() {
        let quietContact = message(#","sender_known":false,"sender_signals_warn":false,"sender_signals":{"reply_to":"pay@elsewhere.example"}"#)
        expect(quietContact?.senderLine?.warning == false, "the daemon said quiet")
        let warned = message(#","sender_known":true,"sender_signals_warn":true,"sender_signals":{"reply_to":"pay@elsewhere.example"}"#)
        expect(warned?.senderLine?.warning == true, "the daemon said warn")
    }

    static func relayAddressesShowTheirDomain() {
        let pm = "e6f85070728474e32e7ac94e8835fd9f+296450@inbound.postmarkapp.com"
        let line = SenderSignals(reply_to: pm).line(senderKnown: false)
        expect(line?.parts == ["Replies go to inbound.postmarkapp.com"], "a routing tag shows its domain: \(line?.parts ?? [])")
        expect(line?.detail == "Replies go to \(pm)", "the tooltip keeps the whole address")
        expect(SenderSignals.readableAddress("reply+8a7d6c5b4e3f2a1b@help.example") == "help.example", "a ticket tag")
        expect(SenderSignals.readableAddress("support@taxfyle.zendesk.com") == "support@taxfyle.zendesk.com", "a person-shaped mailbox stays")
        expect(SenderSignals.readableAddress("jane.doe2024@corp.example") == "jane.doe2024@corp.example", "a few digits stay")
        expect(SenderSignals.readableAddress("no-at-sign") == "no-at-sign", "not an address")
    }

    static func oneBadFieldKeepsTheRest() {
        let m = message(#","sender_signals":{"via":5,"name_spoof":"support@paypal.com","reply_to":["x"]}"#)
        expect(m?.sender_signals?.name_spoof == "support@paypal.com", "the good field survives")
        expect(m?.sender_signals?.via == nil, "the bad one is dropped")
        expect(m?.senderLine?.warning == true, "and the warning still shows")
    }

    static func bidiAndLengthAreCleaned() {
        let s = SenderSignals(reply_to: "a\u{200B}b@x.example", name_spoof: "pay@bank.com\u{202E}gro.live")
        let parts = s.line(senderKnown: false)?.parts ?? []
        expect(parts.allSatisfy { !$0.unicodeScalars.contains { $0.value == 0x202E || $0.value == 0x200B } }, "format characters stripped: \(parts)")
        let long = SenderSignals(via: String(repeating: "a", count: 600))
        expect((long.line(senderKnown: false)?.parts.first?.count ?? 0) < 300, "capped")
    }
}
