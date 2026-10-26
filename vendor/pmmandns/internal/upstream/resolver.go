package upstream

import (
	"context"
	"fmt"
	"time"

	"github.com/miekg/dns"
)

type Resolver struct {
	servers []string
	timeout time.Duration
}

func New(servers []string, timeout time.Duration) *Resolver {
	return &Resolver{servers: servers, timeout: timeout}
}

func (r *Resolver) Exchange(ctx context.Context, req *dns.Msg) (*dns.Msg, error) {
	var last error
	for _, server := range r.servers {
		c := &dns.Client{Net: "udp", Timeout: r.timeout}
		resp, _, err := c.ExchangeContext(ctx, req, server)
		if err == nil && resp.Truncated {
			c.Net = "tcp"
			resp, _, err = c.ExchangeContext(ctx, req, server)
		}
		if err == nil {
			return resp, nil
		}
		last = err
	}
	if last == nil {
		last = fmt.Errorf("no upstream servers")
	}
	return nil, last
}
