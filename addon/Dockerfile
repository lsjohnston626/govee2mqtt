ARG BUILD_FROM
ARG BUILD_VERSION
FROM ghcr.io/lsjohnston626/govee2mqtt:${BUILD_VERSION} AS govee2mqtt
FROM $BUILD_FROM
COPY run.sh /run.sh
COPY --from=govee2mqtt /app/govee /app/
COPY --from=govee2mqtt /app/assets /app/assets/
COPY --from=govee2mqtt /app/AmazonRootCA1.pem /app/
CMD [ "/run.sh" ]
