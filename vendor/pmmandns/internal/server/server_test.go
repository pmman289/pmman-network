package server

import (
	"net"
	"testing"
	"time"

	"github.com/miekg/dns"
	"pmmandns/internal/cache"
	"pmmandns/internal/rules"
	"pmmandns/internal/upstream"
)

type captureWriter struct {
	msg   *dns.Msg
	local net.Addr
}

func (w *captureWriter) LocalAddr() net.Addr       { return w.local }
func (w *captureWriter) RemoteAddr() net.Addr      { return w.local }
func (w *captureWriter) WriteMsg(m *dns.Msg) error { w.msg = m; return nil }
func (w *captureWriter) Write([]byte) (int, error) { return 0, nil }
func (w *captureWriter) Close() error              { return nil }
func (w *captureWriter) TsigStatus() error         { return nil }
func (w *captureWriter) TsigTimersOnly(bool)       {}
func (w *captureWriter) Hijack()                   {}

func startMock(t *testing.T, answerA, answerAAAA bool) string {
	t.Helper()
	h := dns.HandlerFunc(func(w dns.ResponseWriter, r *dns.Msg) {
		m := new(dns.Msg)
		m.SetReply(r)
		q := r.Question[0]
		if q.Qtype == dns.TypeA && answerA {
			m.Answer = []dns.RR{&dns.A{Hdr: dns.RR_Header{Name: q.Name, Rrtype: dns.TypeA, Class: dns.ClassINET, Ttl: 60}, A: net.ParseIP("192.0.2.1")}}
		}
		if q.Qtype == dns.TypeAAAA && answerAAAA {
			m.Answer = []dns.RR{&dns.AAAA{Hdr: dns.RR_Header{Name: q.Name, Rrtype: dns.TypeAAAA, Class: dns.ClassINET, Ttl: 60}, AAAA: net.ParseIP("2001:db8::1")}}
		}
		_ = w.WriteMsg(m)
	})
	s := &dns.Server{Addr: "127.0.0.1:0", Net: "udp", Handler: h}
	pc, err := net.ListenPacket("udp", s.Addr)
	if err != nil {
		t.Fatal(err)
	}
	s.PacketConn = pc
	go s.ActivateAndServe()
	t.Cleanup(func() { _ = s.Shutdown() })
	return pc.LocalAddr().String()
}

func newTestServer(t *testing.T, addr string) *Server {
	t.Helper()
	matcher, _ := rules.New(nil, nil, rules.Domestic)
	return &Server{Matcher: matcher, Domestic: upstream.New([]string{addr}, time.Second), Foreign: upstream.New([]string{addr}, time.Second), Cache: cache.New(10), Timeout: time.Second, MaxTTL: 300, NegativeTTL: 5}
}

func query(t *testing.T, s *Server, typ uint16) *dns.Msg {
	t.Helper()
	req := new(dns.Msg)
	req.SetQuestion("example.com.", typ)
	w := &captureWriter{local: &net.UDPAddr{}}
	s.ServeDNS(w, req)
	return w.msg
}

func TestDomesticPrefersA(t *testing.T) {
	s := newTestServer(t, startMock(t, true, true))
	if m := query(t, s, dns.TypeA); len(m.Answer) != 1 || m.Answer[0].Header().Rrtype != dns.TypeA {
		t.Fatalf("unexpected A response: %v", m)
	}
	if m := query(t, s, dns.TypeAAAA); len(m.Answer) != 0 {
		t.Fatalf("AAAA should be filtered: %v", m.Answer)
	}
}

func TestDomesticPreservesCNAMEChain(t *testing.T) {
	h := dns.HandlerFunc(func(w dns.ResponseWriter, r *dns.Msg) {
		m := new(dns.Msg)
		m.SetReply(r)
		q := r.Question[0]
		if q.Qtype == dns.TypeA {
			m.Answer = []dns.RR{
				&dns.CNAME{Hdr: dns.RR_Header{Name: q.Name, Rrtype: dns.TypeCNAME, Class: dns.ClassINET, Ttl: 60}, Target: "www.example.net."},
				&dns.A{Hdr: dns.RR_Header{Name: "www.example.net.", Rrtype: dns.TypeA, Class: dns.ClassINET, Ttl: 60}, A: net.ParseIP("192.0.2.2")},
			}
		}
		_ = w.WriteMsg(m)
	})
	s := &dns.Server{Addr: "127.0.0.1:0", Net: "udp", Handler: h}
	pc, err := net.ListenPacket("udp", s.Addr)
	if err != nil {
		t.Fatal(err)
	}
	s.PacketConn = pc
	go s.ActivateAndServe()
	t.Cleanup(func() { _ = s.Shutdown() })

	server := newTestServer(t, pc.LocalAddr().String())
	m := query(t, server, dns.TypeA)
	if len(m.Answer) != 2 || m.Answer[0].Header().Rrtype != dns.TypeCNAME || m.Answer[1].Header().Rrtype != dns.TypeA {
		t.Fatalf("CNAME chain was not preserved: %v", m.Answer)
	}
	if m.Answer[0].(*dns.CNAME).Target != "www.example.net." {
		t.Fatalf("unexpected CNAME target: %v", m.Answer[0])
	}
}

func TestDomesticFallsBackToAAAA(t *testing.T) {
	s := newTestServer(t, startMock(t, false, true))
	if m := query(t, s, dns.TypeA); len(m.Answer) != 0 {
		t.Fatalf("A should be empty: %v", m.Answer)
	}
	if m := query(t, s, dns.TypeAAAA); len(m.Answer) != 1 || m.Answer[0].Header().Rrtype != dns.TypeAAAA {
		t.Fatalf("unexpected AAAA response: %v", m)
	}
}

func TestLocalSuffixBoundary(t *testing.T) {
	if !matchesLocalSuffix("router.lan", []string{"lan"}) {
		t.Fatal("expected local suffix match")
	}
	if matchesLocalSuffix("notlan", []string{"lan"}) {
		t.Fatal("unexpected local suffix match")
	}
}

func TestDN42SuffixBoundary(t *testing.T) {
	if !isDN42("map.dn42") || !isDN42("dn42") {
		t.Fatal("expected DN42 names to match")
	}
	if isDN42("notdn42.example") {
		t.Fatal("unexpected DN42 match")
	}
}
