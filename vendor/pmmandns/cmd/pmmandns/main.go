package main

import (
	"flag"
	"log"
	"os"
	"os/signal"
	"syscall"
	"time"

	"github.com/miekg/dns"
	"pmmandns/internal/cache"
	"pmmandns/internal/config"
	"pmmandns/internal/rules"
	"pmmandns/internal/server"
	"pmmandns/internal/upstream"
)

func main() {
	path := flag.String("config", "/etc/pmmandns/config.json", "configuration file")
	flag.Parse()
	c, err := config.Load(*path)
	if err != nil {
		log.Fatal(err)
	}
	def := rules.Foreign
	if c.DefaultGroup == "domestic" {
		def = rules.Domestic
	}
	if c.GeositeDomestic == "" {
		c.GeositeDomestic = "cn"
	}
	if c.GeositeForeign == "" {
		c.GeositeForeign = "geolocation-!cn"
	}
	m, err := rules.NewWithGeosite(c.GeositeFile, c.GeositeDomestic, c.GeositeForeign, c.DomesticRules, c.ForeignRules, def)
	if err != nil {
		log.Fatal(err)
	}
	s := &server.Server{Matcher: m, Domestic: upstream.New(c.DomesticUpstreams, time.Duration(c.TimeoutMS)*time.Millisecond), Foreign: upstream.New(c.ForeignUpstreams, time.Duration(c.TimeoutMS)*time.Millisecond), Cache: cache.New(c.CacheSize), Timeout: time.Duration(c.TimeoutMS) * time.Millisecond, MaxTTL: c.MaxTTL, NegativeTTL: c.NegativeTTL}
	if c.LocalUpstream != "" {
		s.Local = upstream.New([]string{c.LocalUpstream}, time.Duration(c.TimeoutMS)*time.Millisecond)
		s.LocalSuffixes = c.LocalSuffixes
	}
	if len(c.DN42Upstreams) > 0 {
		s.DN42 = upstream.New(c.DN42Upstreams, time.Duration(c.TimeoutMS)*time.Millisecond)
	}
	servers := make([]*dns.Server, 0, len(c.Listen)*2)
	for _, addr := range c.Listen {
		for _, network := range []string{"udp", "tcp"} {
			d := &dns.Server{Addr: addr, Net: network, Handler: s}
			servers = append(servers, d)
			go func(x *dns.Server) {
				if err := x.ListenAndServe(); err != nil {
					log.Printf("dns %s %s: %v", x.Net, x.Addr, err)
				}
			}(d)
		}
	}
	sig := make(chan os.Signal, 1)
	signal.Notify(sig, syscall.SIGINT, syscall.SIGTERM)
	<-sig
	for _, d := range servers {
		_ = d.Shutdown()
	}
}
