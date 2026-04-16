.PHONY: deb deb-client deb-server

deb:
	$(MAKE) deb-server
	$(MAKE) deb-client

deb-server:
	./scripts/build-deb.sh usbip-server

deb-client:
	./scripts/build-deb.sh usbip-client

