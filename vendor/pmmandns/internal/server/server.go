package server

import (
	"context"
	"fmt"
	"strings"
	"time"

	"github.com/miekg/dns"
	"pmmandns/internal/cache"
	"pmmandns/internal/rules"
	"pmmandns/internal/upstream"
)

type Server struct {
	Matcher                  *rules.Matcher
	Domestic, Foreign, Local *upstream.Resolver
	DN42                     *upstream.Resolver
	Cache                    *cache.Cache
	Timeout                  time.Duration
	MaxTTL, NegativeTTL      uint32
	LocalSuffixes            []string
}

func (s *Server) ServeDNS(w dns.ResponseWriter, req *dns.Msg) {
	if len(req.Question) != 1 {
		m := new(dns.Msg)
		m.SetRcode(req, dns.RcodeFormatError)
		_ = w.WriteMsg(m)
		return
	}
	q := req.Question[0]
	name := strings.ToLower(strings.TrimSuffix(q.Name, "."))
	group := s.Matcher.Match(name)
	if s.DN42 != nil && isDN42(name) {
		ctx, cancel := context.WithTimeout(context.Background(), s.Timeout)
		defer cancel()
		m, err := s.DN42.Exchange(ctx, req)
		if err != nil {
			s.fail(w, req)
			return
		}
		m.Id = req.Id
		_ = w.WriteMsg(m)
		return
	}
	if s.Local != nil && matchesLocalSuffix(name, s.LocalSuffixes) {
		ctx, cancel := context.WithTimeout(context.Background(), s.Timeout)
		defer cancel()
		m, err := s.Local.Exchange(ctx, req)
		if err != nil {
			s.fail(w, req)
			return
		}
		m.Id = req.Id
		_ = w.WriteMsg(m)
		return
	}
	if group == rules.Domestic {
		s.domestic(w, req, name)
		return
	}
	key := fmt.Sprintf("%s/%d/%d", name, q.Qtype, q.Qclass)
	if m, ok := s.Cache.GetForeign(key); ok {
		m.Id = req.Id
		_ = w.WriteMsg(m)
		return
	}
	ctx, cancel := context.WithTimeout(context.Background(), s.Timeout)
	defer cancel()
	m, err := s.Foreign.Exchange(ctx, req)
	if err != nil {
		s.fail(w, req)
		return
	}
	s.Cache.PutForeign(key, m)
	m.Id = req.Id
	_ = w.WriteMsg(m)
}

func isDN42(name string) bool {
	return name == "dn42" || strings.HasSuffix(name, ".dn42")
}

func matchesLocalSuffix(name string, suffixes []string) bool {
	for _, suffix := range suffixes {
		suffix = strings.TrimSuffix(strings.ToLower(strings.TrimSpace(suffix)), ".")
		if suffix != "" && (name == suffix || strings.HasSuffix(name, "."+suffix)) {
			return true
		}
	}
	return false
}

func (s *Server) domestic(w dns.ResponseWriter, req *dns.Msg, name string) {
	key := name + "/" + fmt.Sprint(req.Question[0].Qclass)
	entry, ok := s.Cache.GetDomestic(key)
	if !ok {
		entry = s.queryDomestic(req)
		if entry.ExpireAt.IsZero() {
			s.fail(w, req)
			return
		}
		s.Cache.PutDomestic(key, entry)
	}
	m := new(dns.Msg)
	m.SetReply(req)
	m.Rcode = entry.RCode
	if len(entry.A) > 0 {
		if req.Question[0].Qtype == dns.TypeA {
			m.Answer = append(m.Answer, entry.AAnswer...)
		}
	} else if len(entry.AAAA) > 0 && req.Question[0].Qtype == dns.TypeAAAA {
		m.Answer = append(m.Answer, entry.AAAAAnswer...)
	}
	m.Id = req.Id
	_ = w.WriteMsg(m)
}

func (s *Server) queryDomestic(req *dns.Msg) cache.DomesticEntry {
	var ar, aaar *dns.Msg
	var ae, aaae error
	done := make(chan struct{}, 2)
	ctx, cancel := context.WithTimeout(context.Background(), s.Timeout)
	defer cancel()
	go func() {
		r := req.Copy()
		r.Question[0].Qtype = dns.TypeA
		ar, ae = s.Domestic.Exchange(ctx, r)
		done <- struct{}{}
	}()
	go func() {
		r := req.Copy()
		r.Question[0].Qtype = dns.TypeAAAA
		aaar, aaae = s.Domestic.Exchange(ctx, r)
		done <- struct{}{}
	}()
	<-done
	<-done
	if ae != nil && aaae != nil {
		return cache.DomesticEntry{}
	}
	var a, aaaa []dns.RR
	var aAnswer, aaaaAnswer []dns.RR
	rc := dns.RcodeSuccess
	if ar != nil {
		rc = ar.Rcode
		for _, x := range ar.Answer {
			if x.Header().Rrtype == dns.TypeA {
				a = append(a, x)
			}
		}
		aAnswer = answerForType(ar.Answer, dns.TypeA)
	} else if aaar != nil {
		rc = aaar.Rcode
	}
	if aaar != nil {
		for _, x := range aaar.Answer {
			if x.Header().Rrtype == dns.TypeAAAA {
				aaaa = append(aaaa, x)
			}
		}
		aaaaAnswer = answerForType(aaar.Answer, dns.TypeAAAA)
		if len(a) == 0 || len(aaaa) > 0 {
			rc = aaar.Rcode
		}
	}
	ttl := s.NegativeTTL
	for _, x := range append(a, aaaa...) {
		if x.Header().Ttl > 0 && (ttl == s.NegativeTTL || x.Header().Ttl < ttl) {
			ttl = x.Header().Ttl
		}
	}
	return cache.DomesticEntry{A: a, AAAA: aaaa, AAnswer: aAnswer, AAAAAnswer: aaaaAnswer, RCode: rc, ExpireAt: time.Now().Add(time.Duration(min(ttl, s.MaxTTL)) * time.Second)}
}

// answerForType keeps the CNAME chain and terminal records for one address
// family, while filtering the opposite address family from the response.
func answerForType(answer []dns.RR, qtype uint16) []dns.RR {
	selected := make([]dns.RR, 0, len(answer))
	for _, rr := range answer {
		typ := rr.Header().Rrtype
		if typ == dns.TypeCNAME || typ == qtype {
			selected = append(selected, rr)
		}
	}
	return selected
}
func min(a, b uint32) uint32 {
	if a < b {
		return a
	}
	return b
}
func (s *Server) fail(w dns.ResponseWriter, req *dns.Msg) {
	m := new(dns.Msg)
	m.SetRcode(req, dns.RcodeServerFailure)
	_ = w.WriteMsg(m)
}
